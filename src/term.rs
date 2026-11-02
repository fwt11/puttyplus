//! 终端面板（对应 C 版 term.c）：VTE 终端 + spawn ssh/plink 子进程。
//!
//! 包含堡垒机场景的三件套：SSH_ASKPASS 自我调用、PTY 屏幕扫描自动应答、
//! OSC 7 目录跟随回调。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use vte4 as vte;
use vte4::prelude::*;

use crate::args::ConnOptions;
use crate::menu;
use crate::util::find_program_in_path;

const AUTOFEEED_MAX: i32 = 3;
const AUTOFEEED_MS: u64 = 30_000;

/* ---------------- askpass 模式 ---------------- */

/// 自身可执行文件路径，用于 SSH_ASKPASS 自我调用判定。
fn self_exe_path() -> Option<std::path::PathBuf> {
    std::fs::read_link("/proc/self/exe").ok()
}

/// askpass 模式：ssh 会以 [提示文本] 为参数调用本程序并读取 stdout。
/// 判定条件：显式 --askpass，或 SSH_ASKPASS 指向自身且带有密码环境变量。
/// 返回 true 表示已处理（进程已在内部退出）。
pub fn run_askpass(args_after_prog: &[String]) -> bool {
    let pw = std::env::var("PUTTYPLUS_PW").ok();
    let ap = std::env::var("SSH_ASKPASS").ok();

    let mut is_askpass = args_after_prog.len() == 1 && args_after_prog[0] == "--askpass";
    if !is_askpass {
        if let (Some(pw), Some(ap)) = (&pw, &ap) {
            if !pw.is_empty() && !ap.is_empty() {
                if let (Some(selfp), Ok(rap)) = (self_exe_path(), std::fs::canonicalize(ap)) {
                    if rap == selfp {
                        is_askpass = true;
                    }
                }
            }
        }
    }
    if !is_askpass {
        return false;
    }
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "{}\n", pw.as_deref().unwrap_or(""));
    let _ = out.flush();
    // 与 C 版 _exit 一致：不做任何清理
    std::process::exit(0);
}

/* ---------------- 子进程 argv / env 构建 ---------------- */

fn forward_flags(o: &ConnOptions) -> Vec<(String, String)> {
    // forwards 元素形如 "L:9000:127.0.0.1:9000" / "D:1080"
    o.forwards
        .iter()
        .filter_map(|f| {
            let (kind, spec) = f.split_once(':')?;
            let flag = match kind {
                "L" => "-L",
                "R" => "-R",
                _ => "-D",
            };
            Some((flag.to_string(), spec.to_string()))
        })
        .collect()
}

pub fn build_ssh_argv(o: &ConnOptions) -> Option<Vec<String>> {
    let ssh = find_program_in_path("ssh")?;
    let mut av = vec![ssh.to_string_lossy().into_owned()];
    if o.port != 22 {
        av.push("-p".into());
        av.push(o.port.to_string());
    }
    if !o.user.is_empty() {
        av.push("-l".into());
        av.push(o.user.clone());
    }
    if let Some(kf) = &o.keyfile {
        if !kf.is_empty() {
            av.push("-i".into());
            av.push(kf.clone());
        }
    }
    for (flag, spec) in forward_flags(o) {
        av.push(flag);
        av.push(spec);
    }
    if o.no_shell { av.push("-N".into()); }
    if o.force_pty { av.push("-t".into()); }
    if o.no_pty { av.push("-T".into()); }
    if o.ipv4 { av.push("-4".into()); }
    if o.ipv6 { av.push("-6".into()); }
    if o.compress { av.push("-C".into()); }
    if o.verbose { av.push("-v".into()); }
    if o.batch {
        av.push("-o".into());
        av.push("BatchMode=yes".into());
    }
    av.push("-o".into());
    av.push("StrictHostKeyChecking=accept-new".into());
    // NAT/防火墙下的空闲保活（与 SFTP 侧 russh keepalive 对齐）
    av.push("-o".into());
    av.push("ServerAliveInterval=30".into());
    av.push("-o".into());
    av.push("ServerAliveCountMax=3".into());
    if let Some(pc) = &o.proxycmd {
        if !pc.is_empty() {
            av.push("-o".into());
            av.push(format!("ProxyCommand={pc}"));
        }
    }
    av.push("--".into());
    av.push(o.host.clone());
    if let Some(cmd) = &o.remote_cmd {
        if !cmd.is_empty() {
            av.push(cmd.clone());
        }
    }
    Some(av)
}

pub fn build_plink_argv(o: &ConnOptions) -> Option<Vec<String>> {
    let plink = find_program_in_path("plink")?;
    let mut av = vec![plink.to_string_lossy().into_owned()];
    av.push(format!("-{}", o.proto));
    av.push("-P".into());
    av.push(o.port.to_string());
    if !o.user.is_empty() {
        av.push("-l".into());
        av.push(o.user.clone());
    }
    if let Some(pw) = o.password_str() {
        av.push("-pw".into());
        av.push(pw.to_string());
    }
    if let Some(kf) = &o.keyfile {
        if !kf.is_empty() {
            av.push("-i".into());
            av.push(kf.clone());
        }
    }
    for (flag, spec) in forward_flags(o) {
        av.push(flag);
        av.push(spec);
    }
    if o.no_shell { av.push("-N".into()); }
    if o.force_pty { av.push("-t".into()); }
    if o.no_pty { av.push("-T".into()); }
    if o.ipv4 { av.push("-4".into()); }
    if o.ipv6 { av.push("-6".into()); }
    if o.compress { av.push("-C".into()); }
    if o.verbose { av.push("-v".into()); }
    if o.batch { av.push("-batch".into()); }
    av.push(o.host.clone());
    if let Some(cmd) = &o.remote_cmd {
        if !cmd.is_empty() {
            av.push(cmd.clone());
        }
    }
    Some(av)
}

/// telnet/串口：自桥接子进程 argv。
fn build_bridge_argv(o: &ConnOptions) -> Option<Vec<String>> {
    let selfp = std::fs::read_link("/proc/self/exe")
        .or_else(|_| std::env::current_exe())
        .ok()?;
    let selfs = selfp.to_string_lossy().into_owned();
    match o.proto.as_str() {
        "telnet" => Some(vec![
            selfs,
            "--bridge-telnet".into(),
            o.host.clone(),
            o.port.to_string(),
        ]),
        "serial" => {
            // sercfg 与 PuTTY -sercfg 兼容：波特率[,数据位[,校验[,停止位[,流控]]]]
            let sercfg = format!(
                "{},{},{},{},{}",
                o.serial_baud, o.serial_data, o.serial_parity, o.serial_stop, o.serial_flow
            );
            Some(vec![selfs, "--bridge-serial".into(), o.host.clone(), sercfg])
        }
        "raw" => {
            let nc = find_program_in_path("nc")?;
            Some(vec![
                nc.to_string_lossy().into_owned(),
                o.host.clone(),
                o.port.to_string(),
            ])
        }
        _ => None,
    }
}

pub fn build_env(o: &ConnOptions) -> Vec<String> {
    let mut env: Vec<String> = std::env::vars_os()
        .map(|(k, v)| format!("{}={}", k.to_string_lossy(), v.to_string_lossy()))
        .collect();
    let mut set = |k: &str, v: String| {
        env.retain(|e| !e.starts_with(&format!("{k}=")));
        env.push(format!("{k}={v}"));
    };
    set(
        "TERM",
        o.term_type.clone().unwrap_or_else(|| "xterm-256color".into()),
    );
    set("COLORTERM", "truecolor".into());

    // 有密码、无密钥时启用 askpass 自动应答（OpenSSH >= 8.4）
    if o.password_str().map_or(false, |p| !p.is_empty())
        && o.keyfile.as_deref().map_or(true, |k| k.is_empty())
        && o.is_ssh()
    {
        if let Some(selfp) = self_exe_path() {
            set("SSH_ASKPASS", selfp.to_string_lossy().into_owned());
            set("SSH_ASKPASS_REQUIRE", "force".into());
            set("PUTTYPLUS_PW", o.password_str().unwrap().to_string());
            if std::env::var("DISPLAY").is_err() {
                set("DISPLAY", ":0".into());
            }
        }
    }
    env
}

/* ---------------- 终端面板 ---------------- */

/// 右键菜单扩展区：在弹出的菜单底部追加控件。
/// 第二个参数是当前面板的内部句柄（勾选项要按它自己的状态初始化）。
///
/// 刻意只传 `&Rc<TermInner>` 而不是 `TermPane`：闭包若抓着 pane，
/// 就成 inner → 闭包 → pane → inner 的环，面板永远回收不掉。
pub(crate) type MenuExtra = Rc<dyn Fn(&gtk::Box, &Rc<TermInner>)>;

/// 广播钩子：把编码好的按键字节交给 tabs，由它决定发给哪些 pane。
/// 刻意不捕获 pane 本身——否则 inner → 闭包 → pane → inner 成环，
/// 面板永远不会被回收。
pub(crate) type BroadcastHook = Box<dyn Fn(&Rc<TermInner>, &[u8])>;

pub(crate) struct TermInner {
    term: vte::Terminal,
    search_bar: gtk::SearchBar,
    search_entry: gtk::Entry,
    /// 查找条的「上一个」按钮（阶段 84）
    search_prev_btn: gtk::Button,
    /// 查找条的「下一个」按钮（阶段 84）
    search_next_btn: gtk::Button,
    /// 状态条容器（3.2 广播 / 3.4 日志 / 6.1 录制，三行都是按需显示）
    banner: gtk::Box,
    banner_bc: gtk::Button,
    banner_log: gtk::Label,
    /// 录制指示条（做成按钮：点了就停录制，和广播条一个交互逻辑）
    banner_rec: gtk::Button,
    /// 「会话已结束 · 点此重新连接」条（阶段 25）
    ///
    /// **为什么要显式摆出来**：ssh 会话结束（用户敲了 exit、网络断了）
    /// 之后，终端就只是个"死画面"——光在终端里打一行字，用户未必知道
    /// 还能救。右键菜单里有「重新连接」，但那得用户**自己想到右键**。
    /// 会话一断就把按钮摆出来，救回来只要一下。
    banner_re: gtk::Button,
    /// 右键菜单的扩展区提供方（分屏/广播/日志由 tabs 注册）
    menu_extra: RefCell<Option<MenuExtra>>,
    /// **会话就绪钩子**：ssh 子进程起来之后调一次。
    ///
    /// 「在此处打开终端」要用：新终端连上后往 pty 里写 `cd <目录>`，
    /// 让它进到用户右键的那个目录。时机必须等"子进程起来了"——
    /// `feed_keys` 在 `child_alive` 还是 false 时**写不进去**（静默丢）。
    ///
    /// ⚠️ 钩子拿到的是**终端**，不是 TermPane：闭包一旦捕获 TermPane，
    /// 就是「TermPane → ready_hook → TermPane」的**引用环**，面板永远
    /// 释放不掉（AGENTS.md 的红线）。
    ready_hook: RefCell<Option<Box<dyn Fn(&vte::Terminal) + 'static>>>,
    /// 按键广播钩子（全局开关装在每个 pane 上）
    key_sink: RefCell<Option<BroadcastHook>>,
    /// **会话存活钩子**（阶段 65 / 审计 P0-4）：子进程起来 / 退出时调一次。
    ///
    /// 参数：`true` = 会话活着；`false` = 已结束。
    ///
    /// ⚠️ 闭包只能抓**弱引用**（面板 / 标签），抓强引用就成环
    /// （TermPane → 闭包 → TermPane ⇒ 面板永远释放不掉）。
    alive_hook: RefCell<Option<Box<dyn Fn(bool) + 'static>>>,
    /// **是否已经把「存活」报给上层**（阶段 65）。
    ///
    /// 干什么用：`set_alive_hook` 装钩子时若子进程**已经**起来了就补报一次
    /// 「存活」，而 spawn 的回调只在**没报过**时才发 ⇒ 不会重复计数
    /// （重复计数 ⇒ 终端掉了一个也还剩一个 ⇒ 状态永远死不掉）。
    ///
    /// ⚠️ 当前时序里 spawn 是**异步**的（`pty.spawn_async`）⇒ 回调在主
    /// 循环里跑，那时钩子早装好了 ⇒ 正常路径**用不到**补报。
    /// 这里是**防御**：万一哪天 spawn 改成同步、或钩子装得更晚，
    /// 状态也不会永远停在「未知」。
    alive_reported: Cell<bool>,
    /// 终端日志（3.4）
    log: RefCell<Option<TermLog>>,
    /// 会话录制（6.1）
    rec: RefCell<Option<TermRec>>,
    /// 子进程是否还活着——广播不给已退出的会话写字节
    child_alive: Cell<bool>,
    opts: ConnOptions,
    /* PTY 自动应答状态 */
    autofeed_on: Cell<bool>,
    autofeeds: Cell<i32>,
    autofeed_deadline: Cell<u64>, // 单调毫秒
    last_line_fed: RefCell<Option<String>>,
    /* OSC 7 终端目录回调 */
    cwd_cb: RefCell<Option<Box<dyn Fn(&str)>>>,
    /* 远程标题（OSC 0/2 → window-title 属性）回调 */
    title_cb: RefCell<Option<Box<dyn Fn(&str)>>>,
    spawn_wall: Cell<i64>,
}

#[derive(Clone)]
pub struct TermPane {
    inner: Rc<TermInner>,
    box_: gtk::Box,
}

impl TermInner {
    /// 直接 spawn 时的子进程 argv（ssh / plink / 自桥接）。
    /// 分屏的每个 pane 都各自走这一条（彼此独立）。
    fn ssh_argv(&self) -> Vec<String> {
        let o = &self.opts;
        if o.is_ssh() {
            // 装了 putty 优先用 plink（与 putty 参数行为完全一致）
            if find_program_in_path("plink").is_some() {
                build_plink_argv(o).unwrap_or_default()
            } else {
                build_ssh_argv(o).unwrap_or_default()
            }
        } else {
            // telnet/串口：自桥接（子进程 = 本程序，PTY stdio ↔ TCP/串口）
            build_bridge_argv(o).unwrap_or_default()
        }
    }
}

impl TermPane {
    /// 终端缓冲区搜索条（Ctrl+F 切换；Enter/Shift+Enter 前后查找；Esc 关闭）。
    pub fn toggle_search(&self, open: bool) {
        if open {
            self.inner.search_bar.set_search_mode(true);
            self.inner.search_entry.grab_focus();
        } else {
            self.inner.search_bar.set_search_mode(false);
            self.inner.term.grab_focus();
        }
    }

    /// 查找条上的「上一个 / 下一个」按钮（冒烟 / 测试钩子）。
    ///
    /// ⚠️ 以前只能靠回车跳上一个，鼠标用户没辙（阶段 84 补的）。
    pub fn find_nav_buttons(&self) -> Vec<(&'static str, gtk::Button)> {
        vec![
            ("上一个", self.inner.search_prev_btn.clone()),
            ("下一个", self.inner.search_next_btn.clone()),
        ]
    }

    /// 面板获得焦点/被点击时回调（分屏时据此判定"往哪块分"与高亮）。
    pub fn set_focus_callback<F: Fn() + 'static>(&self, f: F) {
        let f = Rc::new(f);
        // 键盘/焦点切换
        let c = gtk::EventControllerFocus::new();
        let f1 = Rc::clone(&f);
        c.connect_enter(move |_| f1());
        self.inner.term.add_controller(c);
        // 鼠标点进来。VTE 会自己吃掉 button-press（选择/粘贴），
        // GestureClick 收不到，所以用 legacy 控制器旁听原始事件（不改变传播）。
        let legacy = gtk::EventControllerLegacy::new();
        let f2 = Rc::clone(&f);
        legacy.connect_event(move |_, ev| {
            if ev.event_type() == gtk::gdk::EventType::ButtonPress {
                f2();
            }
            gtk::glib::Propagation::Proceed
        });
        self.inner.term.add_controller(legacy);
    }

    /// 一个独立终端：独占一条 SSH 连接（分屏的每个 pane 各来一个）。
    pub fn new(opts: &ConnOptions) -> TermPane {
        let term = vte::Terminal::new();

        // ⚠️ 阶段 69：终端字号的**全局默认**（首选项中心 →「终端」页）。
        // 优先级：连接里设的字号 > 全局默认 > 终端内置的 10.5。
        // 以前只有前两档 ⇒ 想"把终端字体调大"得逐条连接去改（审计 P1-4）。
        set_appearance(
            &term,
            opts.font_size.or_else(crate::settings::term_font_size),
            opts.bell,
        );
        term.set_mouse_autohide(true);
        term.set_size(100, 32);
        term.set_scrollback_lines(10_000);
        apply_colors(&term);

        // 搜索条（默认收起；Ctrl+F 呼出）
        //
        // ⚠️ 用的是**共用构件** `findbar`（阶段 84）：跟编辑器的查找条同一套
        // 外观与按键约定（Enter 下一个 / Shift+Enter 上一个 / Esc 关闭），
        // 两边各画一套的话很快就会打架。
        // 终端这边只要"输入框 + 上一个/下一个"——VTE 的搜索不报匹配数，
        // 也没有"替换"（回滚缓冲改不了）。
        let fb = crate::findbar::build(crate::findbar::FindOpts::minimal());
        let search_entry = fb.find_entry.clone();
        search_entry.set_placeholder_text(Some("在终端中查找…"));
        {
            let term2 = term.clone();
            search_entry.connect_changed(move |e| {
                let pat = regex_escape(&e.text());
                let rx = vte::Regex::for_match(&pat, 0).ok();
                term2.search_set_regex(rx.as_ref(), 0);
            });
        }
        {
            let term2 = term.clone();
            search_entry.connect_activate(move |_| {
                if !term2.search_find_next() {
                    // 单结果时 next 失败再回绕上一个
                    term2.search_find_previous();
                }
            });
            // 查找说明文字从快捷键表取（4.4），不在这里另写一份
            {
                let e = search_entry.clone();
                if let Some(b) = crate::shortcuts::by_display("Shift+Enter") {
                    e.set_tooltip_text(Some(&format!(
                        "回车：下一个（{}
{}：上一个）",
                        next_hint(),
                        b.display
                    )));
                }
            }
        }
        {
            // Shift+Enter = 上一个
            let term2 = term.clone();
            let ek = gtk::EventControllerKey::new();
            ek.connect_key_pressed(move |_k, keyval, _c, mods| {
                if keyval.name().as_deref() == Some("Return")
                    && mods.contains(gdk::ModifierType::SHIFT_MASK)
                {
                    term2.search_find_previous();
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            });
            search_entry.add_controller(ek);
        }
        {
            // 「上一个 / 下一个」按钮（阶段 84）：以前只能靠回车，
            // 鼠标用户想跳到上一个匹配只能自己数回车按几次。
            let term2 = term.clone();
            fb.prev_btn.connect_clicked(move |_| {
                term2.search_find_previous();
            });
            let term2 = term.clone();
            fb.next_btn.connect_clicked(move |_| {
                if !term2.search_find_next() {
                    term2.search_find_previous();
                }
            });
        }
        let search_bar = gtk::SearchBar::new();
        search_bar.set_child(Some(&fb.root));
        search_bar.set_show_close_button(true);
        search_bar.connect_entry(&search_entry);
        {
            // Esc 关闭后焦点回终端
            let term2 = term.clone();
            search_bar.connect_notify_local(Some("search-mode-enabled"), move |bar, _| {
                if !bar.is_search_mode() {
                    term2.grab_focus();
                }
            });
        }

        let search_bar2 = search_bar.clone();
        let search_entry2 = search_entry.clone();
        let search_prev2 = fb.prev_btn.clone();
        let search_next2 = fb.next_btn.clone();

        // 状态条（3.2 广播 / 3.4 日志）：整块常驻控件，按需显示其中的行。
        // 广播会劫持所有按键、日志会在磁盘上长文件，提示必须显眼到
        // "想关的时候一定找得到"。
        // 状态提示做成一排 chips：广播（黄）/ 日志（中性）/ 录制（红）。
        // 原先是三行竖排的 flat 按钮 + 纯文字，高危状态看着和普通灰字没区别。
        let banner_bc = gtk::Button::new();
        banner_bc.add_css_class("pp-chip");
        banner_bc.add_css_class("pp-chip-warn");
        banner_bc.set_valign(gtk::Align::Center);
        banner_bc.set_tooltip_text(Some("点击停止广播（Alt+Shift+B）"));
        banner_bc.connect_clicked(|_| crate::tabs::set_broadcast(false));
        let banner_log = gtk::Label::new(None);
        banner_log.add_css_class("pp-chip");
        banner_log.set_xalign(0.0);
        banner_log.set_valign(gtk::Align::Center);
        banner_log.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        // 6.1 录制：同样是"开着就要一眼看得见、想关一下就能关"。
        // 点击回调要抓 inner，而 inner 在下面才建 —— 先建控件，
        // 回调等 inner 出来再连。
        let banner_rec = gtk::Button::new();
        banner_rec.add_css_class("pp-chip");
        banner_rec.add_css_class("pp-chip-err");
        banner_rec.set_valign(gtk::Align::Center);
        // 会话结束后的「重新连接」条（阶段 25）：默认不显示，断了才出现
        let banner_re = gtk::Button::new();
        banner_re.add_css_class("pp-chip");
        banner_re.add_css_class("pp-chip-warn");
        banner_re.set_valign(gtk::Align::Center);
        banner_re.set_label("会话已结束 · 点此重新连接");
        banner_re.set_tooltip_text(Some(
            "重新建立这条 SSH 会话（和右键菜单里的「重新连接终端」是同一个动作）",
        ));
        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        banner.set_margin_top(4);
        banner.set_margin_bottom(2);
        banner.set_margin_start(8);
        banner.set_margin_end(8);
        banner.set_halign(gtk::Align::Start);
        // ⚠️ **每个 chip 都要先把自己的可见标志置 false**。
        //
        // 状态条整体是"按需显示"的（没提示时整条藏起来），而判断用哪个
        // API 至关重要：
        //   - `is_visible()` 是**有效可见性**（自身标志 + 所有祖先都可见）；
        //   - `get_visible()` 才是**自己的可见标志**。
        // 用 `is_visible()` 的话，父容器一隐藏，子 chip 的有效可见性恒为
        // false → 永远算不出"有提示" → **广播/日志/录制的指示条从来就没
        // 显示过**（用户只看到终端里多一行字）。
        // 所以：标志用 `get_visible()` 读，用 `set_visible()` 写。
        banner_bc.set_visible(false);
        banner_log.set_visible(false);
        banner_rec.set_visible(false);
        banner_re.set_visible(false);
        banner.append(&banner_bc);
        banner.append(&banner_log);
        banner.append(&banner_rec);
        banner.append(&banner_re);
        banner.set_visible(false);

        let inner = Rc::new(TermInner {
            term: term.clone(),
            search_bar: search_bar2,
            search_entry: search_entry2,
            search_prev_btn: search_prev2,
            search_next_btn: search_next2,
            banner,
            banner_bc,
            banner_log,
            banner_rec,
            banner_re,
            menu_extra: RefCell::new(None),
            ready_hook: RefCell::new(None),
            key_sink: RefCell::new(None),
            log: RefCell::new(None),
            rec: RefCell::new(None),
            child_alive: Cell::new(false),
            alive_hook: RefCell::new(None),
            alive_reported: Cell::new(false),
            opts: opts.clone(),
            autofeed_on: Cell::new(false),
            autofeeds: Cell::new(0),
            autofeed_deadline: Cell::new(0),
            last_line_fed: RefCell::new(None),
            cwd_cb: RefCell::new(None),
            title_cb: RefCell::new(None),
            spawn_wall: Cell::new(0),
        });

        // 不挂滚动条：回滚只用鼠标滚轮（wezterm / deepin-terminal 的分屏
        // 也是这样），终端因此能用到面板的最后一列像素。VTE 自己处理滚轮。
        // VTE 终端必须显式 expand，否则最大化后 Box 剩余空间留白在右侧
        term.set_hexpand(true);
        term.set_vexpand(true);

        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // 分屏后每块面板的最小可用尺寸（再小就没法看终端了）
        box_.set_size_request(160, 100);
        box_.set_hexpand(true);
        box_.set_vexpand(true);
        box_.append(&inner.banner);
        box_.append(&search_bar);
        box_.append(&term);

        connect_signals(&inner);
        install_broadcast_capture(&inner);
        // 录制指示条点一下就停（和广播条同一个交互逻辑）
        {
            let weak = Rc::downgrade(&inner);
            inner.banner_rec.connect_clicked(move |_| {
                if let Some(i) = weak.upgrade() {
                    if i.rec.borrow().is_some() {
                        i.stop_recording();
                        i.sync_rec_ui();
                    }
                }
            });
        }
        // 「会话已结束」条：点一下就重建这条 SSH 会话（阶段 25）
        //
        // 刻意用 Weak：闭包抓着 Rc<TermInner> 会成 inner → 闭包 → inner 的环，
        // 面板永远回收不掉。
        {
            let weak = Rc::downgrade(&inner);
            inner.banner_re.connect_clicked(move |_| {
                if let Some(i) = weak.upgrade() {
                    i.reconnect_term();
                }
            });
        }
        spawn_term(&inner);
        TermPane { inner, box_ }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.box_.upcast_ref()
    }

    pub fn terminal(&self) -> &vte::Terminal {
        &self.inner.term
    }

    pub fn restart(&self) {
        // VTE spawn 不自动清屏，重连时清掉上一会话的残留
        self.inner.reconnect_term();
    }

    /// OSC 7 目录变化回调（file:// URI）。
    pub fn set_cwd_callback<F: Fn(&str) + 'static>(&self, cb: F) {
        *self.inner.cwd_cb.borrow_mut() = Some(Box::new(cb));
    }

    /// 远程标题变化回调（shell/程序经 OSC 0/2 设置标题时触发）。
    pub fn set_title_callback<F: Fn(&str) + 'static>(&self, cb: F) {
        *self.inner.title_cb.borrow_mut() = Some(Box::new(cb));
    }

    /// ssh 子进程启动时刻（Unix 秒），供面板目录跟随轮询匹配。
    pub fn spawn_time(&self) -> i64 {
        self.inner.spawn_wall.get()
    }

    /* ---- 输入广播（3.2） ---- */

    /// 内部句柄：广播时用来判断"是不是同一个 pane"。
    pub(crate) fn handle(&self) -> &Rc<TermInner> {
        &self.inner
    }

    /// 重新建立这条 SSH 会话（ssh 进程重开，终端清屏）。
    pub fn reconnect_term(&self) {
        self.inner.reconnect_term();
    }

    /// 冒烟测试用：「会话已结束 · 点此重新连接」条现在是不是摆着的。
    ///
    /// 读自己的可见标志（`get_visible`），不是有效可见性——冒烟里终端
    /// 面板没挂到窗口上，有效可见性恒为 false。
    pub fn reconnect_banner_visible(&self) -> bool {
        self.inner.banner_re.get_visible()
    }

    /// 冒烟测试用：ssh 子进程还活着吗（会话结束 → false）。
    pub fn session_alive(&self) -> bool {
        self.inner.child_alive.get()
    }

    /// 装"会话存活"钩子（阶段 65 / 审计 P0-4）。
    ///
    /// 子进程起来 / 退出时各调一次（`true` / `false`）。
    ///
    /// ⚠️ 闭包**只能抓弱引用**（面板 / 标签）——抓强引用就是引用环
    /// （TermPane → 钩子 → TermPane），面板永远释放不掉。
    ///
    /// ⚠️ **装的时候子进程可能已经起来了**：这时要**补报**一次「存活」，
    /// 否则上层永远停在「未知」（`alive_reported` 保证不与 spawn 回调
    /// 重复计数）。
    pub fn set_alive_hook<F: Fn(bool) + 'static>(&self, f: F) {
        *self.inner.alive_hook.borrow_mut() = Some(Box::new(f));
        // 补报：子进程还活着 ⇒ 告诉上层「活着」
        if self.inner.child_alive.get() {
            self.inner.alive_reported.set(true);
            if let Some(h) = self.inner.alive_hook.borrow().as_ref() {
                h(true);
            }
        }
    }

    /// 按当前配色方案重刷本面板（4.1）。切方案时对所有面板调一次。
    pub fn apply_palette(&self) {
        apply_palette(&self.inner.term, crate::theme::palette());
    }

    /// 装/卸按键广播钩子。钩子捕获不到 self，按键只以字节形式出去。
    pub(crate) fn set_broadcast_hook(&self, h: Option<BroadcastHook>) {
        *self.inner.key_sink.borrow_mut() = h;
    }

    /// 登记右键菜单的扩展区（分屏/广播/日志）。
    pub fn set_menu_extra(&self, f: MenuExtra) {
        *self.inner.menu_extra.borrow_mut() = Some(f);
    }

    /// **会话就绪钩子**：ssh 子进程起来之后调一次（只调一次）。
    ///
    /// 给「在此处打开终端」用——新终端连上后要 `cd` 到用户右键的目录。
    ///
    /// ⚠️ 闭包里**别捕获 TermPane 自己**（那是自引用，终端永远释放不掉）。
    /// 钩子收到的是终端本身，往里写字节就行。
    pub fn set_ready_hook<F: Fn(&vte::Terminal) + 'static>(&self, f: F) {
        *self.inner.ready_hook.borrow_mut() = Some(Box::new(f));
    }

    /// 子进程已退出的 pane 不接广播——写进去的字节没人读，
    /// 用户看到的现象是"这个终端莫名其妙没反应"。
    pub(crate) fn feed_keys(&self, b: &[u8]) {
        if self.inner.child_alive.get() {
            self.inner.term.feed_child(b);
        }
    }

    /// 广播状态同步到本面板的 UI：状态条 + 右键菜单里的勾选项。
    pub fn sync_broadcast_ui(&self, on: bool, targets: usize) {
        let text = if targets > 0 {
            format!("⤷ 广播中 · 按键同时发往其它 {targets} 个终端（点击停用）")
        } else {
            "⤷ 广播中 · 暂无其它终端可接收（点击停用）".to_string()
        };
        self.inner.banner_bc.set_label(&text);
        self.inner.banner_bc.set_visible(on);
        self.refresh_banner();
    }

    /// 两行状态条整体是否可见（都关时别占地方）。
    fn refresh_banner(&self) {
        self.inner.sync_banner_visibility();
    }
}

/* ---------------- 终端日志落盘（3.4） ----------------
 *
 * 抓的是**屏幕文字**，不是 PTY 原始字节流：VTE 没有导出原始输出，
 * 我们 spawn 时也拿不到 master fd（VTE 自己读它）。所以做法与 PuTTY 的
 * "Printable output" 同一路子——按屏幕行增量记账。
 *
 * 怎么对齐前后两次快照：终端的行为是"滚屏 + 在末尾追加"，所以取
 * **prev 的某个后缀 == cur 的前缀**（取最长的那个），其后即为新出现的
 * 内容。命令行回显被改写、命令输出插在老提示符前面这些都能对上。
 *
 * 固有边界（UI 与文档都写明）：两次采样之间滚过一屏以上、或终端被清屏，
 * 会漏记；同一行被程序反复改写（正在输入的命令）会按每次变化记一行。
 */

/// 日志轮询间隔（毫秒）。取样再密也不见得更准，屏幕快照本身就是全量重建。
const LOG_POLL_MS: u64 = 700;

/// 正在进行的终端日志。
struct TermLog {
    file: std::fs::File,
    path: std::path::PathBuf,
    /// 上一次屏幕快照（按行）
    prev: Vec<String>,
    /// glib 定时器，关标签时要摘掉——定时器闭包抓着 Rc<TermInner>，
    /// 不摘就是一条实打实的引用环
    src: glib::SourceId,
}

/// 前后两次屏幕快照 → 需要追加写进日志的行。
///
/// 取最长的 "prev 后缀 == cur 前缀" 对齐点；末尾的空白行是屏幕底部留白，
/// 不是内容，丢掉（`clear` 之后因此不会刷一屏空行）。
pub fn new_lines(prev: &[String], cur: &[String]) -> Vec<String> {
    let mut best = 0usize;
    for d in 0..prev.len() {
        let mut k = 0usize;
        while d + k < prev.len() && k < cur.len() && prev[d + k] == cur[k] {
            k += 1;
        }
        if k > best {
            best = k;
        }
    }
    let mut out: Vec<String> = cur[best.min(cur.len())..].to_vec();
    while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        out.pop();
    }
    out
}

/// 会话名 → 文件名安全片段。
pub fn log_stem(session: &str) -> String {
    let mut out = String::with_capacity(session.len().min(48));
    let mut last_us = true;
    for c in session.chars() {
        if c.is_control() || matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
            out.push('_');
            last_us = true;
        } else if c.is_whitespace() {
            if !last_us {
                out.push('_');
            }
            last_us = true;
        } else {
            out.push(c);
            last_us = false;
        }
    }
    let t = out.trim_matches('_');
    if t.is_empty() {
        "session".to_string()
    } else {
        t.chars().take(48).collect()
    }
}

/// 默认日志目录：`<用户数据目录>/puttyplus/logs`。
pub fn default_log_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("PUTTYPLUS_LOG_DIR") {
        if !d.trim().is_empty() {
            return std::path::PathBuf::from(d);
        }
    }
    glib::user_data_dir().join("puttyplus").join("logs")
}

/// 默认录制目录：`<用户数据目录>/puttyplus/captures`。
///
/// 与日志目录分开：用户翻日志时不该被一堆 .cast 淹掉，反过来也一样。
pub fn default_capture_dir() -> std::path::PathBuf {
    if let Ok(d) = std::env::var("PUTTYPLUS_CAPTURE_DIR") {
        if !d.trim().is_empty() {
            return std::path::PathBuf::from(d);
        }
    }
    glib::user_data_dir().join("puttyplus").join("captures")
}

/// 录制目录里时间最新的 `.cast` 文件名（没有就 None）。
///
/// 只取文件名不给全路径：菜单 tooltip 里显示全路径太长。
pub fn latest_capture_name() -> Option<String> {
    let dir = default_capture_dir();
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for e in std::fs::read_dir(&dir).ok()? {
        let Ok(e) = e else { continue };
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("cast") {
            continue;
        }
        let Ok(t) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if best.as_ref().is_none_or(|(bt, _)| t > *bt) {
            best = Some((t, p.to_string_lossy().into_owned()));
        }
    }
    best.map(|(_, p)| p)
}

/// 会话标题（用户看到的那一串）。
fn session_stem(opts: &ConnOptions) -> String {
    match opts.title.as_deref() {
        Some(t) if !t.trim().is_empty() => t.to_string(),
        _ => format!("{}@{}:{}", opts.user, opts.host, opts.port),
    }
}

/// 往日志文件追加若干行并立刻 flush——进程被杀也要留下已发生的部分。
fn log_write(log: &mut TermLog, lines: &[String]) {
    use std::io::Write;
    let mut buf = String::new();
    for l in lines {
        buf.push_str(l);
        buf.push('\n');
    }
    if buf.is_empty() {
        return;
    }
    if log.file.write_all(buf.as_bytes()).and_then(|_| log.file.flush()).is_err() {
        eprintln!("[日志] 写入 {} 失败，已停止记录", log.path.display());
    }
}

/// 日志轮询：取一次屏幕快照，扣掉上次已记的行，剩下的追加进文件。
fn log_poll(inner: &Rc<TermInner>) -> glib::ControlFlow {
    let snapshot: Vec<String> = inner
        .term
        .text_format(vte::Format::Text)
        .map(|s| s.lines().map(str::to_string).collect())
        .unwrap_or_default();
    let mut slot = inner.log.borrow_mut();
    let Some(log) = slot.as_mut() else {
        return glib::ControlFlow::Break;
    };
    let lines = new_lines(&log.prev, &snapshot);
    log.prev = snapshot;
    log_write(log, &lines);
    glib::ControlFlow::Continue
}

impl TermPane {
    /// 开关本终端的日志。Err 是打不开文件的原因（直接给用户看）。
    /// 返回开启后的状态（失败时维持原状）。
    pub fn toggle_log(&self) -> Result<bool, String> {
        if self.inner.log.borrow().is_some() {
            self.stop_log();
            self.sync_log_ui();
            return Ok(false);
        }
        match self.start_log() {
            Ok(()) => {
                self.sync_log_ui();
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn start_log(&self) -> Result<(), String> {
        let dir = default_log_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败：{e}"))?;
        let path = dir.join(format!(
            "{}-{}.log",
            log_stem(&session_stem(&self.inner.opts)),
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("打开日志文件失败：{e}"))?;
        let head = format!(
            "# PuTTYPlus 终端日志\n# 会话: {}\n# 开始: {}\n",
            session_stem(&self.inner.opts),
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S")
        );
        use std::io::Write;
        let _ = file.write_all(head.as_bytes());

        // 先把当前屏幕作为基线记下来：用户往往是"看到出错才想起要开日志"
        let prev: Vec<String> = self
            .inner
            .term
            .text_format(vte::Format::Text)
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default();

        let src = glib::timeout_add_local(std::time::Duration::from_millis(LOG_POLL_MS), {
            let inner = Rc::clone(&self.inner);
            move || log_poll(&inner)
        });
        *self.inner.log.borrow_mut() = Some(TermLog {
            file,
            path,
            prev,
            src,
        });
        Ok(())
    }

    /// 停日志并摘掉定时器（定时器闭包抓着本面板，不摘会一直跑）。
    pub fn stop_log(&self) {
        if let Some(log) = self.inner.log.borrow_mut().take() {
            log.src.remove();
        }
    }

    pub fn log_path(&self) -> Option<String> {
        self.inner.log_path()
    }

    /// 日志开关状态同步到状态条与右键菜单。
    pub fn sync_log_ui(&self) {
        match self.log_path() {
            Some(p) => {
                self.inner.banner_log.set_text(&format!("⤷ 记录中 → {p}"));
                self.inner.banner_log.set_tooltip_text(Some(
                    "终端屏幕文字逐行落盘。同一行被反复改写会记多行；\
                     两次采样之间滚过一屏以上或被清屏会漏记——抓的是屏幕，不是原始字节流。",
                ));
                self.inner.banner_log.set_visible(true);
            }
            None => self.inner.banner_log.set_visible(false),
        }
        self.refresh_banner();
    }

    /// 面板退出前收尾（停日志/录制/定时器）。关标签时必须调，
    /// 否则定时器把面板钉住、录制文件也一直开着。
    pub fn shutdown(&self) {
        self.stop_log();
        self.stop_recording();
    }
}

impl TermInner {
    /// 状态条整体是否可见（一个提示都没有时别占地方）。
    ///
    /// **必须读 `get_visible()`（自己的标志），不能读 `is_visible()`**：
    /// 后者是"有效可见性"（自身 + 祖先都可见），而状态条整体在没提示时
    /// 是藏起来的——用 `is_visible()` 的话，每个 chip 的有效可见性恒为
    /// false，永远算不出"有提示"，指示条就永远不显示。
    ///
    /// 放在 TermInner 上而不是 TermPane：点「重新连接」条的闭包只抓着
    /// `Rc<TermInner>`（抓 pane 会成 inner → 闭包 → pane → inner 的环）。
    pub(crate) fn sync_banner_visibility(&self) {
        let on = self.banner_bc.get_visible()
            || self.banner_log.get_visible()
            || self.banner_rec.get_visible()
            || self.banner_re.get_visible();
        self.banner.set_visible(on);
    }

    /// 重新建立这条 SSH 会话（ssh 进程重开，终端清屏）。
    ///
    /// 用 `self: &Rc<Self>`：重连要重新 spawn 子进程，而 `spawn_term`
    /// 收的是 `&Rc<TermInner>`。
    pub(crate) fn reconnect_term(self: &Rc<Self>) {
        // 新会话一起来，「会话已结束」那条就该收起来
        self.banner_re.set_visible(false);
        self.sync_banner_visibility();
        // VTE spawn 不自动清屏，重连时清掉上一会话的残留
        self.term.reset(true, true);
        spawn_term(self);
    }

    /// 当前日志文件路径（没在记录时 None）。
    pub(crate) fn log_path(&self) -> Option<String> {
        self.log
            .borrow()
            .as_ref()
            .map(|l| l.path.display().to_string())
    }

    /// 当前录制文件路径（没在录时 None）。
    pub(crate) fn rec_path(&self) -> Option<String> {
        self.rec
            .borrow()
            .as_ref()
            .map(|r| r.rec.path().display().to_string())
    }

    /// 录制期间因为输出太快、被滚动缓冲挤掉的行数（没在录时 0）。
    pub(crate) fn rec_lost(&self) -> usize {
        self.rec.borrow().as_ref().map(|r| r.lost).unwrap_or(0)
    }

    /// 停录制并摘掉定时器。
    pub(crate) fn stop_recording(&self) {
        if let Some(r) = self.rec.borrow_mut().take() {
            r.src.remove();
            r.rec.stop();
        }
    }

    /// 录制指示条的显示状态。
    ///
    /// 放在 TermInner 上而不是 TermPane：点"停止"的那个闭包只抓着
    /// `Rc<TermInner>`（抓 pane 会成 inner → 闭包 → pane → inner 的环）。
    pub(crate) fn sync_rec_ui(&self) {
        match self.rec_path() {
            Some(p) => {
                // 丢了行就明说：录制横幅最怕"看着在录、其实啥也没录到"
                let lost = self.rec_lost();
                let lost_note = if lost > 0 {
                    format!("· 已丢 {lost} 行")
                } else {
                    String::new()
                };
                let l = gtk::Label::new(Some(&format!(
                    "● 录制中 → {p}（点此停止）{lost_note}"
                )));
                l.add_css_class("pp-hint");
                l.set_xalign(0.0);
                l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                self.banner_rec.set_child(Some(&l));
                self.banner_rec.set_tooltip_text(Some(rec_banner_tooltip()));
                self.banner_rec.set_visible(true);
            }
            None => self.banner_rec.set_visible(false),
        }
        self.sync_banner_visibility();
    }
}

/* ---------------- 会话录制回放（6.1） ----------------
 *
 * 输出侧与 3.4 日志同一路子（VTE 不导出原始输出，只能定期抓屏幕），
 * 但有两点不一样：
 *   1. 抓的是**带颜色**的屏幕（`text_range_format(Html)` → 转 ANSI），
 *      落盘的是标准 asciicast v2，`asciinema play` 也能直接播；
 *   2. 采样更密（250ms 对 700ms），录制漏得少一些。
 *
 * **输入侧不录**（不写 `i` 事件）：
 *   · 用户敲的命令**照样出现在录制的输出里**，而且是**逐次重写**录下来的
 *     （阶段 88）：光标行每变一次就重写一次（`\r` + 清行 + 行文本），
 *     所以回放里看得到命令是**一个个敲出来的**，不靠输入事件；
 *   · 而输入事件里会有**明文密码**：密码输入时远端把 echo 关掉，
 *     那串字节不会回传（我们这边却抓得到）⇒ 落盘就是事故。
 *     asciicast v2 规范也是这么说的（官方 asciinema 默认不录输入，
 *     "实现不应默认录输入，除非用户明确要求"）。
 *   代价：远端关掉回显的场景（vim / less / `su` / 嵌套 ssh）里，
 *   敲的按键不会出现在任何地方——这是可以接受的取舍。
 *
 * 边界（UI 与文档都写明）：
 *   · **正在敲的那一行**（光标行）现在会**逐次重写**（`\r` + 清行 + 行文本），
 *     所以回放里能看到命令是**一个个敲出来的**——和 asciinema 的观感一致；
 *     但**已经输出完的行**被反复改写，仍只留**最后一次**（那不是敲的过程，
 *     是远端在重画，例如进度条）；
 *   · 敲命令的**粒度是采样间隔**（250ms），不是按键级——按键级得录输入事件，
 *     而输入事件里会有明文密码（见上）；
 *   · 终端被清屏（`clear`）会漏；
 *   · 一次输出快过采样间隔、又把滚动缓冲挤爆时，那部分会丢——
 *     丢多少会写在状态条横幅上，不装作"全录到了"。
 *
 * 与 asciinema 的差距（它录的是 PTY 原始字节流，100% 保真）：
 * 这里是**屏幕快照**。光标行已经逐次重写（敲的过程能看到了），
 * 但**别处的** \r 覆写（进度条）、TUI 重绘仍只留最终画面。
 */

/// 录制采样间隔（毫秒）。比日志密——日志漏一行无所谓，录制漏屏很难看。
const REC_POLL_MS: u64 = 250;

/// 正在进行的会话录制。
struct TermRec {
    rec: crate::replay::Recorder,
    /// 采样器（光标行的基线与"重写"状态都在里面）
    sampler: RecSampler,
    /// 上一次采样时的屏幕尺寸，用于发 `r` 事件
    cols: i64,
    rows: i64,
    /// 输出太快、被滚动缓冲挤掉而**没录到**的行数
    lost: usize,
    /// glib 定时器，关标签时要摘掉（闭包抓着 Rc<TermInner>，不摘是引用环）
    src: glib::SourceId,
}

/// VTE 的**光标绝对行号**。
///
/// ⚠⚠ 两个坑，踩过就懂：
///   1. `cursor_position()` 返回 **(列, 行)**，顺序反直觉。写反了会把列当行用，
///      症状是"录到的全是空行"。VTE 官方文档原话：
///      *"The row coordinate is absolute."*（行号是绝对的，从终端启动起算）
///   2. `text_range_format(0, 0, rows-1, cols-1)` 取到的**不是屏幕**，
///      而是**会话历史最前面的 rows 行**——那几行早就滚走了，
///      之后再也不会变 ⇒ 采样多少次都是"没变化" ⇒ **什么也录不下来**。
///      （实测：一次性 2000 行输出，老实现录到 **0 行**。）
pub fn cursor_row(term: &vte::Terminal) -> i64 {
    term.cursor_position().1
}

/// 取绝对行号区间 `[start, end]` 的**带颜色**屏幕行（每行一段 ANSI）。
///
/// 行数 = `end - start + 1`。区间前头若超出滚动缓冲保留范围，
/// 那几行取回来是**空串**——那部分是真丢了（`rec_poll` 里在数）。
pub fn ansi_rows(term: &vte::Terminal, start: i64, end: i64) -> Vec<String> {
    if end < start {
        return Vec::new();
    }
    let cols = term.column_count().max(1);
    let (html, _) = term.text_range_format(vte::Format::Html, start, 0, end, cols - 1);
    match html {
        Some(h) => crate::replay::ansi_lines(&h),
        None => Vec::new(),
    }
}

/// 敲完一行（光标下移）时，那一行该怎么处理（**纯函数**，好单测）。
///
/// 背景：光标行（正在敲的那行）现在会**逐次重写**录下来 ⇒ 敲完回车时，
/// 那一行**已经在屏幕上显示过了**，批量再录一遍就会**出现两遍**。
/// 但"敲得比采样快"（一个采样周期内敲完）时，那一行**从没显示过** ⇒
/// 必须照旧批量录进去，否则命令会凭空消失。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DoneLine {
    /// 从没显示过 ⇒ 照旧批量录进去
    RecordInBatch,
    /// 显示过、且已是最终态 ⇒ 批量里跳过它
    Skip,
    /// 显示过、但不是最终态（最后几下没赶上采样）⇒ 先补一次重写，再跳过
    RewriteThenSkip,
}

/// 见 [`DoneLine`]。
fn done_line_action(shown: bool, shown_text: &str, done_text: &str) -> DoneLine {
    if !shown {
        DoneLine::RecordInBatch
    } else if shown_text == done_text {
        DoneLine::Skip
    } else {
        DoneLine::RewriteThenSkip
    }
}

/// 重写一行用的字节：`\r`（回到列 0）+ `\x1b[K`（清到行尾）+ 行文本。
///
/// 和 asciinema 录到的原始字节流里一模一样（它录的是 `\r\u001b[K`）⇒
/// 回放端用**真 VTE** 喂这些字节，效果完全一致（我们的回放窗就是这么播的）。
fn rewrite_line(text: &str) -> String {
    format!("\r\x1b[K{text}")
}

/* ---------------- 录制采样器（单测/冒烟能单独驱动） ---------------- */

/**
 * 录制采样器：把**屏幕的变化**转成 `.cast` 里的输出字节。
 *
 * 抽成独立构件，是为了**能单独驱动**——单测和冒烟都直接拿它跑，
 * 不必等 GTK 定时器（定时器在无头环境里不好等）。
 *
 * 两条规则：
 *   1. **光标没动** ⇒ 用户在敲命令 ⇒ 光标行变了就**重写**它
 *      （`\r` + 清行 + 行文本）⇒ 回放里命令是**一个个敲出来的**；
 *   2. **光标下移** ⇒ 上一次的光标行"敲完了" ⇒ 它可能已经显示过了
 *      （别再录一遍，否则屏幕上会出现两遍），新出现的行照旧批量录。
 */
pub struct RecSampler {
    /// 上一次采样时的**光标绝对行号**（VTE 的行号从终端启动起算）
    prev_row: i64,
    /// 当前**光标行**（正在敲的那一行）是不是已经以"重写"方式录过了。
    ///
    /// 录过了 ⇒ 敲完回车时，那一行**不能**再批量录一遍（屏幕上会出现两遍）。
    cur_shown: bool,
    /// 上一次重写光标行时的**行文本**（用来判"这次变了没"）。
    cur_text: String,
}

impl RecSampler {
    /// 从某一行开始采样（一般是**开始录制时**的光标行）。
    pub fn new(start_row: i64) -> RecSampler {
        RecSampler {
            prev_row: start_row,
            cur_shown: false,
            cur_text: String::new(),
        }
    }

    /// 重建基线（尺寸变了 / 光标往回走了时用）：之前的行都不再管。
    pub fn reset(&mut self, row: i64) {
        self.prev_row = row;
        self.cur_shown = false;
        self.cur_text.clear();
    }

    /// 采一次样，把新增/重写的内容写进录制文件。
    ///
    /// 返回**本次丢掉**的行数（输出太快、被滚动缓冲挤掉了）。
    pub fn sample(&mut self, term: &vte::Terminal, rec: &mut crate::replay::Recorder) -> usize {
        let cur = cursor_row(term);
        if cur < self.prev_row {
            // 光标行号往回走了（`clear` 之类）：重建基线，别把旧行再录一遍。
            // ⚠ 回放端的游标还停在原处 ⇒ 之后的行会**整体错位**——
            //   这是"录屏幕而不是字节流"的固有代价（`clear` 本来就录不准）。
            self.reset(cur);
            return 0;
        }
        if cur == self.prev_row {
            // 光标没动 ⇒ 用户在**敲命令** ⇒ 光标行变了就**重写**它。
            //
            // ⚠ 为什么是"重写"而不是"追加"：正在敲的那行是**同一行**，
            // 追加会变成一行行往下堆（屏幕上看着像刷了一屏命令）。
            // 重写用 `\r` + 清行，和真终端一个路数 ⇒ 回放端（真 VTE）还原。
            let text = ansi_rows(term, cur, cur).first().cloned().unwrap_or_default();
            // 空行且从没显示过 ⇒ 什么都不用录（刚起手，光标后面还没东西）
            if text != self.cur_text && !(text.is_empty() && !self.cur_shown) {
                rec.output_raw(&rewrite_line(&text));
                self.cur_text = text;
                self.cur_shown = true;
            }
            return 0;
        }
        // 光标下移了 ⇒ 上一次的光标行"敲完了"
        //
        // 一次采样间隔内输出超过滚动缓冲容量时，区间前头会被挤掉，
        // 取回来是空串——那部分是真丢了，得如实告诉用户
        let keep = term.scrollback_lines().max(1) - 1;
        let oldest = (cur - keep).max(0);
        let lost = (oldest - self.prev_row).max(0) as usize;
        // 敲完的那一行 = 上一次采样时的光标行（= prev_row）
        let done_row = self.prev_row;
        let done_text = ansi_rows(term, done_row, done_row)
            .first()
            .cloned()
            .unwrap_or_default();
        match done_line_action(self.cur_shown, &self.cur_text, &done_text) {
            DoneLine::RecordInBatch => {
                // 这行从没显示过（敲得比采样快）⇒ 照旧批量录进去
                let lines = ansi_rows(term, done_row, cur - 1);
                rec.output(&lines);
            }
            DoneLine::Skip => {
                // 显示过、且已是最终态 ⇒ 批量里跳过它；
                // 但**光标得挪到下一行**（回放端的游标还停在那行）
                rec.output_raw("\r\n");
                let lines = ansi_rows(term, done_row + 1, cur - 1);
                rec.output(&lines);
            }
            DoneLine::RewriteThenSkip => {
                // 显示过、但不是最终态 ⇒ 先把最后几下补上（重写一次），
                // 再挪到下一行
                rec.output_raw(&rewrite_line(&done_text));
                rec.output_raw("\r\n");
                let lines = ansi_rows(term, done_row + 1, cur - 1);
                rec.output(&lines);
            }
        }
        self.prev_row = cur;
        // 新的光标行还没录过
        self.cur_shown = false;
        self.cur_text.clear();
        lost
    }
}

/// 录制采样：抓一次**新渲染出来的行**，写进 `.cast`。
///
/// 靠光标的**绝对行号**判断"哪些行是新的"：光标行号从 `prev_row` 涨到
/// `cur`，说明这中间渲染了 `[prev_row, cur-1]` 这些行。
///
/// 为什么不比"整屏快照"：整屏快照只能看到屏幕，滚过去的行没了；
/// 而按行号区间去取，**滚动缓冲里还留着**的行也能取回来（带颜色）。
fn rec_poll(inner: &Rc<TermInner>) -> glib::ControlFlow {
    let cols = inner.term.column_count().max(1);
    let rows = inner.term.row_count().max(1);
    let cur = cursor_row(&inner.term);
    // 借用只在这段里，别跨出去——`sync_rec_ui` 还要再借一次同一个 RefCell
    let mut lost_now = 0usize;
    {
        let mut slot = inner.rec.borrow_mut();
        let Some(r) = slot.as_mut() else {
            return glib::ControlFlow::Break;
        };
        if cols != r.cols || rows != r.rows {
            // 尺寸变了：先记一条 `r`，并把基线整个换掉——
            // 重排会让行号对不上，拿旧行号去取行会取到不相干的内容
            r.rec.resize(cols, rows);
            r.cols = cols;
            r.rows = rows;
            r.sampler.reset(cur);
            return glib::ControlFlow::Continue;
        }
        {
            // 采样与写文件在同一个借用里，别跨出去——
            // `sync_rec_ui` 还要再借一次同一个 RefCell
            let sampler = &mut r.sampler;
            let rec = &mut r.rec;
            lost_now = sampler.sample(&inner.term, rec);
        }
        r.lost += lost_now;
        if r.rec.is_broken() {
            eprintln!("[录制] 写入 {} 失败，已停止记录", r.rec.path().display());
        }
    }
    // 丢行了就把横幅上的「已丢 N 行」刷新出来
    if lost_now > 0 {
        inner.sync_rec_ui();
    }
    glib::ControlFlow::Continue
}

impl TermPane {
    /// 开关本终端的录制。Err 是打不开文件的原因（直接给用户看）。
    /// 返回开启后的状态（失败时维持原状）。
    pub fn toggle_recording(&self) -> Result<bool, String> {
        if self.inner.rec.borrow().is_some() {
            self.stop_recording();
            self.sync_rec_ui();
            return Ok(false);
        }
        match self.start_recording() {
            Ok(()) => {
                self.sync_rec_ui();
                Ok(true)
            }
            Err(e) => Err(e),
        }
    }

    fn start_recording(&self) -> Result<(), String> {
        let dir = default_capture_dir();
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建录制目录失败：{e}"))?;
        let path = dir.join(format!(
            "{}-{}.cast",
            log_stem(&session_stem(&self.inner.opts)),
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let cols = self.inner.term.column_count().max(1);
        let rows = self.inner.term.row_count().max(1);
        // 基线 = 开始录制时的光标行号。之后每采样一次，
        // 光标行号涨到哪儿就是录到哪儿（见 `rec_poll`）。
        let prev_row = cursor_row(&self.inner.term);
        let rec = crate::replay::Recorder::start(
            &path,
            cols,
            rows,
            &session_stem(&self.inner.opts),
        )
        .map_err(|e| format!("打开录制文件失败：{e}"))?;
        // 起手就打个标记：回放时一眼看出这段录的是什么
        let mut rec = rec;
        rec.marker(&session_stem(&self.inner.opts));

        let src = glib::timeout_add_local(
            std::time::Duration::from_millis(REC_POLL_MS),
            {
                let inner = Rc::clone(&self.inner);
                move || rec_poll(&inner)
            },
        );
        *self.inner.rec.borrow_mut() = Some(TermRec {
            rec,
            sampler: RecSampler::new(prev_row),
            cols,
            rows,
            lost: 0,
            src,
        });
        Ok(())
    }

    /// 停录制并摘掉定时器。
    pub fn stop_recording(&self) {
        self.inner.stop_recording();
    }

    pub fn rec_path(&self) -> Option<String> {
        self.inner.rec_path()
    }

    pub fn is_recording(&self) -> bool {
        self.inner.rec.borrow().is_some()
    }

    /// 录制开关状态同步到状态条与右键菜单。
    pub fn sync_rec_ui(&self) {
        self.inner.sync_rec_ui();
    }
}

/// 在终端上装按键捕获（捕获阶段，先于 VTE 自己的处理）。
///
/// 只读键、不吞键：编码后交给广播钩子，然后一律 Proceed，
/// 焦点所在终端的行为与没开广播时逐字节一致。
fn install_broadcast_capture(inner: &Rc<TermInner>) {
    let kc = gtk::EventControllerKey::new();
    kc.set_propagation_phase(gtk::PropagationPhase::Capture);
    // 只抓 Weak：controller 挂在终端控件上，抓强的就成环
    let weak = Rc::downgrade(inner);
    kc.connect_key_pressed(move |_k, keyval, _code, mods| {
        let Some(inner) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        if let Some(bytes) = encode_key(keyval, mods) {
            // ⚠ 6.1 录制**不录输入**（asciicast v2 规范："实现不应默认录输入"）。
            //   · 用户敲的命令**本来就会出现在录制的输出里**——远端 tty 的回显
            //     本身就是输出字节，回放里照样看得到敲了什么；
            //   · 而输入事件里会有**明文密码**：密码输入时远端把 echo 关掉，
            //     那串字节根本不会回传（我们这边却抓得到）⇒ 录下来就是事故。
            // 钩子内部只写别的面板的 PTY，不再碰 key_sink，借用期间不会重入
            if let Some(h) = inner.key_sink.borrow().as_ref() {
                h(&inner, &bytes);
            }
        }
        glib::Propagation::Proceed
    });
    inner.term.add_controller(kc);
}

fn connect_signals(inner: &Rc<TermInner>) {
    let term = inner.term.clone();

    // 会话结束提示
    {
        let inner = Rc::clone(inner);
        term.connect_child_exited(move |_t, status| {
            inner.autofeed_on.set(false);
            inner.child_alive.set(false);
            // asciicast v3 的 `x` 事件：会话退出码（回放时看得出是正常退出还是出错退出）
            if let Some(r) = inner.rec.borrow_mut().as_mut() {
                r.rec.exit(status as i64);
            }
            inner.term.feed(
                "\r\n\x1b[1;33m== 会话已结束（状态码见上）。点上方「重新连接」可重建会话 ==\x1b[0m\r\n"
                    .as_bytes(),
            );
            // 阶段 25：会话一断就把「重新连接」摆出来。
            // 光在终端里打一行字，多数用户看不出"还能救"。
            inner.banner_re.set_visible(true);
            inner.sync_banner_visibility();
            // 阶段 65：会话结束了 → 通知上层（标签状态点不再显示绿点）
            //
            // ⚠️ 只在**报过存活**时才报结束：没报过就不用减计数
            // （否则面板的存活数会变成 -1，被 clamp 成 0，
            //   「起过没有」这个事实也就丢了）。
            if inner.alive_reported.replace(false) {
                if let Some(h) = inner.alive_hook.borrow().as_ref() {
                    h(false);
                }
            }
        });
    }

    // 远程标题（OSC 0/2）→ 回调（标签头/窗口标题）
    {
        let inner = Rc::clone(inner);
        term.connect_window_title_notify(move |t| {
            #[allow(deprecated)]
            let title = t.window_title();
            if let Some(title) = title.filter(|s| !s.is_empty()) {
                if let Some(cb) = inner.title_cb.borrow().as_ref() {
                    cb(&title);
                }
            }
        });
    }

    // OSC 7 目录跟随
    {
        let inner = Rc::clone(inner);
        term.connect_termprop_changed(None, move |t, prop| {
            if prop != "vte.current.directory" {
                return;
            }
            #[allow(deprecated)]
            if let Some(uri) = t.current_directory_uri() {
                if !uri.is_empty() {
                    if let Some(cb) = inner.cwd_cb.borrow().as_ref() {
                        cb(&uri);
                    }
                }
            }
        });
    }

    install_context_menu(inner);
    install_middle_click_paste(inner);
}

/* ---------------- 按键编码（广播用） ----------------
 *
 * vte4 没有"某个键刚被按下"的信号（VTE 也没有导出
 * vte_terminal_translate_keyboard_input），所以只能自己把
 * GTK 的 keyval+modifier 折成终端字节序列，喂给其它 pane 的 PTY。
 *
 * 折算规则照 xterm / VTE 的 modifyOtherKeys=0 语义：
 *   - 功能键 → 转义序列，带修饰键时在序列里插 `;1+m` 参数
 *   - 字母 → Ctrl 折成 0x01..0x1f，Alt 前置 ESC
 *   - VTE 自己截获的窗口级组合（复制/粘贴/缩放）不是"输入"，不广播
 *
 * 已知边界（UI 与文档都写明了）：
 *   - 光标键按 CSI 形式发（ESC [ A），不跟随目标终端的 DECCKM。
 *     远端 vi/less 这类开了应用光标键的程序要按应用光标键才认；
 *     老式/新式两种 CSI 形式都收，所以怎么发都能被识别。
 *   - 死键/输入法组合由 VTE 自己合成，我们只看最终字符——
 *     取不到字符（to_unicode 为 None）就不广播，不猜。
 */

/// VTE 自身截获的组合键（复制/粘贴/全选/缩放）：是窗口动作不是输入，
/// 广播出去只会在别的终端里凭空多出一次 Ctrl+C。
fn is_vte_local(keyval: gdk::Key, mods: gdk::ModifierType) -> bool {
    let n = keyval.name();
    let n = n.as_deref().unwrap_or("");
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);
    // 缩放：Ctrl(+Shift)+ / - / 0
    if ctrl && matches!(n, "+" | "-" | "=" | "0" | "plus" | "minus" | "equal") {
        return true;
    }
    // 复制 / 粘贴 / 全选
    if ctrl && shift && matches!(n, "c" | "C" | "v" | "V" | "a" | "A") {
        return true;
    }
    // Ctrl+Insert = 复制，Shift+Insert = 粘贴
    keyval == gdk::Key::Insert && (ctrl || shift)
}

/// 特殊键 → (基础序列, 是否接受修饰键参数)。
/// 小键盘的方向/Home/End 等在 NumLock 关时 GDK 就报成 KP_*，
/// 和主键区同义，这里一并映射。
fn special_seq(keyval: gdk::Key) -> Option<(&'static str, bool)> {
    use gdk::Key as K;
    Some(match keyval {
        k if k == K::Return || k == K::KP_Enter => ("\r", false),
        k if k == K::Tab => ("\t", false),
        k if k == K::ISO_Left_Tab => ("\x1b[Z", false),
        k if k == K::BackSpace => ("\x7f", false),
        k if k == K::Escape => ("\x1b", false),
        k if k == K::Up || k == K::KP_Up => ("\x1b[A", true),
        k if k == K::Down || k == K::KP_Down => ("\x1b[B", true),
        k if k == K::Right || k == K::KP_Right => ("\x1b[C", true),
        k if k == K::Left || k == K::KP_Left => ("\x1b[D", true),
        k if k == K::Home || k == K::KP_Home => ("\x1b[H", true),
        k if k == K::End || k == K::KP_End => ("\x1b[F", true),
        k if k == K::Insert || k == K::KP_Insert => ("\x1b[2~", true),
        k if k == K::Delete || k == K::KP_Delete => ("\x1b[3~", true),
        k if k == K::Page_Up || k == K::KP_Page_Up => ("\x1b[5~", true),
        k if k == K::Page_Down || k == K::KP_Page_Down => ("\x1b[6~", true),
        k if k == K::F1 => ("\x1bOP", true),
        k if k == K::F2 => ("\x1bOQ", true),
        k if k == K::F3 => ("\x1bOR", true),
        k if k == K::F4 => ("\x1bOS", true),
        k if k == K::F5 => ("\x1b[15~", true),
        k if k == K::F6 => ("\x1b[17~", true),
        k if k == K::F7 => ("\x1b[18~", true),
        k if k == K::F8 => ("\x1b[19~", true),
        k if k == K::F9 => ("\x1b[20~", true),
        k if k == K::F10 => ("\x1b[21~", true),
        k if k == K::F11 => ("\x1b[23~", true),
        k if k == K::F12 => ("\x1b[24~", true),
        _ => return None,
    })
}

/// xterm 的修饰键参数：1 + Shift(1) + Alt(2) + Ctrl(4)。
fn mod_param(ctrl: bool, alt: bool, shift: bool) -> usize {
    1 + usize::from(shift) + 2 * usize::from(alt) + 4 * usize::from(ctrl)
}

/// 把基础序列按 xterm 规则插进修饰键参数。
fn with_modifier(base: &str, m: usize) -> String {
    if m <= 1 {
        return base.to_string();
    }
    if let Some(rest) = base.strip_prefix("\x1b[") {
        if let Some(head) = rest.strip_suffix('~') {
            return format!("\x1b[{head};{m}~"); // ESC [ 3 ; 5 ~
        }
        if let Some(ch) = rest.chars().last() {
            return format!("\x1b[1;{m}{ch}"); // ESC [ 1 ; 5 A
        }
    }
    if let Some(rest) = base.strip_prefix("\x1bO") {
        return format!("\x1b[1;{m}{rest}"); // SS3 形式统一升成 CSI
    }
    base.to_string()
}

/// 一个按键 → 喂给子进程的字节。None = 不构成输入（纯修饰键、
/// VTE 自己截获的组合、拿不到字符的按键）。
pub(crate) fn encode_key(keyval: gdk::Key, mods: gdk::ModifierType) -> Option<Vec<u8>> {
    if is_vte_local(keyval, mods) {
        return None;
    }
    let ctrl = mods.contains(gdk::ModifierType::CONTROL_MASK);
    let alt = mods.contains(gdk::ModifierType::ALT_MASK);
    let shift = mods.contains(gdk::ModifierType::SHIFT_MASK);

    if let Some((base, modifiable)) = special_seq(keyval) {
        if !modifiable {
            return Some(base.as_bytes().to_vec());
        }
        return Some(with_modifier(base, mod_param(ctrl, alt, shift)).into_bytes());
    }

    // 其余走字符路径：Shift 已经反映在 keyval 上（GDK 在事件阶段就折算过）
    let ch = keyval.to_unicode()?;
    let mut out: Vec<u8> = Vec::with_capacity(5);
    if alt {
        out.push(0x1b); // ESC 前缀
    }
    let c = ch as u32;
    if !ctrl {
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    } else if c == 0x20 {
        out.push(0x00); // Ctrl+空格 = NUL
    } else if c == 0x3f {
        out.push(0x7f); // Ctrl+? = DEL
    } else if (0x40..=0x7e).contains(&c) {
        // Ctrl+@ … Ctrl+~：折掉高位。范围要含小写字母（0x61..=0x7a）——
        // 按住 Ctrl 时 GDK 报的仍是 GDK_KEY_c(0x63) 而不是 0x03，
        // 只判 0x40..=0x5f 会让 Ctrl+C 漏成普通 'c'。
        out.push((c & 0x1f) as u8);
    } else {
        // Ctrl 折不动它（Ctrl+1 之类）：原样发，与 VTE 一致
        let mut buf = [0u8; 4];
        out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
    }
    Some(out)
}

/* ---------------- 右键菜单里的说明文案 ---------------- */

/// 「录制会话到 .cast」右键菜单项的说明。
///
/// ⚠️ **文案必须与真实行为一致**（用户 2026-11-01：「录屏的 tooltip 与现在
/// 的真实情况显然已经是不符合了」）。旧文案写着"输出是带颜色的屏幕快照、
/// **输入是编码后的按键字节**"——那是阶段 87 之前的行为：**现在输入事件
/// 根本不录**（明文密码不落盘）⇒ 照旧说会让人以为文件里存着自己敲的
/// 密码；而且写出的是 **asciicast v3**（只有**读入**才兼容 v2）。
pub fn rec_menu_tooltip(dir: &std::path::Path) -> String {
    format!(
        "把这个终端录成 asciicast v3（{} 下的 会话名-时间戳.cast）。\n\
         录的是屏幕渲染出来的行（带颜色），不是原始字节流：\n\
         · 你敲的命令会一个个敲出来（光标行逐次重写）；\n\
         · 输入事件不录，所以文件里不会有你敲的密码；\n\
         · 已经输出完的行被反复改写只留最后一次（进度条这类）；\n\
         · 终端被清屏会漏；输出快过采样间隔时可能丢行（横幅上会写丢了多少行）。\n\
         分屏的两个终端各自录制；互不影响。",
        dir.display()
    )
}

/// 「打开 .cast 回放…」右键菜单项的说明。
pub fn replay_menu_tooltip() -> &'static str {
    "挑一个 asciicast v2 / v3 文件回放（也可以直接用 asciinema play 播）。\n\
     回放只是把录下来的屏幕重放一遍，不会真的把按键发出去。"
}

/// 录制指示条（横幅）的说明。
pub fn rec_banner_tooltip() -> &'static str {
    "把会话录成 asciicast v3（.cast），右键终端「打开 .cast 回放」可回放。\n\
     录的是屏幕渲染出来的行（带颜色），不是原始字节流：\n\
     · 你敲的命令会一个个敲出来（光标行逐次重写）；\n\
     · 输入事件不录，所以文件里不会有你敲的密码；\n\
     · 已经输出完的行被反复改写只留最后一次（进度条这类）；\n\
     · 终端被清屏会漏；\n\
     · 输出快过采样间隔、把滚动缓冲挤爆时会丢（横幅上会写丢了多少行）。\n\
     Ctrl+Shift+O 随时打开回放窗口。"
}

/* ---------------- 右键菜单 / 中键粘贴 ---------------- */

/// 终端右键菜单：**整份菜单只有这一个**。
///
/// 早先这里是两个 popover（VTE 自带的复制/粘贴/重新连接 + 分屏菜单各挂一个
/// 右键手势），结果 VTE 那个先弹出来抢了 grab，分屏菜单根本弹不出来——
/// 功能等于没有。合并成一份之后不会再有抢 grab 的问题。
fn next_hint() -> &'static str {
    "Enter"
}

fn install_context_menu(inner: &Rc<TermInner>) {
    let term = inner.term.clone();
    let pop = menu::popover(&term);

    let vbox = menu::body();

    // 直接调 VTE 的方法即可：原来那份 action group 是给 PopoverMenu 用的，
    // 换成 Popover + Box 之后没人会再去找它
    let plain = |icon: &str, label: &str, accel: Option<&str>| {
        let b = menu::item(icon, label, accel);
        // 点条目后收起菜单（Popover 不会因为点内部按钮而自动关）
        let pop2 = pop.clone();
        b.connect_clicked(move |_| pop2.popdown());
        vbox.append(&b);
        b
    };
    // 菜单按钮也只抓 Weak<TermInner>：抓终端本身会连成环
    let weak_btns = Rc::downgrade(inner);
    let b_copy = plain("edit-copy-symbolic", "复制", Some("Ctrl+Shift+C"));
    {
        let w = weak_btns.clone();
        b_copy.connect_clicked(move |_| {
            if let Some(i) = w.upgrade() {
                i.term.copy_clipboard_format(vte::Format::Text);
            }
        });
    }
    let b_paste = plain("edit-paste-symbolic", "粘贴", Some("Ctrl+Shift+V"));
    {
        let w = weak_btns.clone();
        b_paste.connect_clicked(move |_| {
            if let Some(i) = w.upgrade() {
                <vte::Terminal as vte::TerminalExt>::paste_clipboard(&i.term);
            }
        });
    }
    vbox.append(&menu::sep());
    let b_re = plain("view-refresh-symbolic", "重新连接终端", None);
    b_re.set_tooltip_text(Some(
        "把这个终端的会话断开重连一次（ssh 子进程会被杀掉再起一个）。\n\
         会话已经断了的话，屏幕上那条「会话已结束」也可以直接点。",
    ));
    {
        let inner2 = Rc::clone(inner);
        b_re.connect_clicked(move |_| {
            inner2.reconnect_term();
        });
    }

    // 扩展区（分屏 / 广播 / 日志 / 录制）单独放一块，每次弹出**先清空**再重建。
    // 以前直接往 vbox 里 append 且从不清：第 2 次右键菜单长一倍，第 3 次高过
    // 屏幕干脆弹不出来——表现出来就是"右键时灵时不灵、最后没反应"。
    let extra_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    vbox.append(&extra_box);

    pop.set_child(Some(&vbox));

    let click = gtk::GestureClick::new();
    click.set_button(3);
    // 闭包只抓 Weak<TermInner>：抓强的会连成
    // term widget → controller → Rc<TermInner> → term widget 的环，
    // 关掉标签也回收不掉这块面板
    let weak = Rc::downgrade(inner);
    click.connect_pressed(move |g, _n, x, y| {
        if g.current_button() != 3 {
            return;
        }
        let Some(inner) = weak.upgrade() else { return };
        // 右键哪块就聚焦哪块：分屏目标与活动描边跟着走
        inner.term.grab_focus();
        // 扩展区每次弹出重建：勾选状态要跟真实状态一致。
        // 先清掉上一次的控件，否则会一次次往上叠（见 extra_box 处的注释）。
        let extra = inner.menu_extra.borrow().clone();
        if let Some(f) = extra {
            while let Some(c) = extra_box.first_child() {
                extra_box.remove(&c);
            }
            f(&extra_box, &inner);
        }
        let r = gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1);
        pop.set_pointing_to(Some(&r));
        pop.popup();
    });
    term.add_controller(click);
}

fn install_middle_click_paste(inner: &Rc<TermInner>) {
    let term = inner.term.clone();
    let gesture = gtk::GestureClick::new();
    gesture.set_button(2);
    gesture.connect_pressed(|g, _n, _x, _y| {
        if g.current_button() == 2 {
            if let Some(t) = term_of_gesture(g) {
                <vte::Terminal as vte::TerminalExt>::paste_clipboard(&t);
            }
        }
    });
    term.add_controller(gesture);
}

/// 从控件自身反查它所属的终端。
///
/// 闭包里抓一份 `vte::Terminal` 看着省事，实则构成
/// 控件 → controller → 闭包 → 控件 的强引用环，关掉标签也回收不掉。
fn term_of_gesture(g: &gtk::GestureClick) -> Option<vte::Terminal> {
    g.widget().and_downcast::<vte::Terminal>()
}

/* ---------------- 颜色 ---------------- */

/// 字号与响铃（1.5 会话级覆盖）。两处都要即时生效：开标签时一次，
/// 会话选项对话框改完再一次。
///
/// ⚠️ 字号优先级：**会话/连接里设的 > 全局默认（首选项中心）> 内置 10.5**。
/// 全局兜底放在这里（而不是每个调用点各自拼）——会话选项改完要**立即**
/// 生效，那条路径也得用全局默认值，否则"没设过字号"的会话在改一次响铃后
/// 字号会跳回 10.5。
pub fn set_appearance(term: &vte::Terminal, font_size: Option<f64>, bell: Option<bool>) {
    let font_env = std::env::var("PUTTYPLUS_FONT").unwrap_or_default();
    let font = if font_env.is_empty() {
        let size = font_size
            .or_else(crate::settings::term_font_size)
            .unwrap_or(10.5);
        format!("Monospace {size}")
    } else {
        font_env
    };
    term.set_font(Some(&gtk::pango::FontDescription::from_string(&font)));
    term.set_audible_bell(bell.unwrap_or(true));
}

/// 往终端右键菜单尾部追加本面板的条目（分屏 / 广播 / 日志）。
///
/// 菜单本体在 `install_context_menu` 里，只有那一份；这里注册的是一个
/// "往里加控件"的回调，由 tabs 在弹菜单时调用——拿到 `page` 才能反查
/// 标签（`split_page` 要用），而 TermPane 自己不知道自己在哪个标签里。
pub fn attach_split_menu(pane: &TermPane, page: &gtk::Widget) {
    let page = page.clone();
    let pane_menu = pane.clone();
    pane.set_menu_extra(Rc::new(move |vbox: &gtk::Box, inner: &Rc<TermInner>| {
        // vbox 的 parent 是 Popover 内部的 GtkPopoverContent，往上找真正的 Popover
        let pop_owner = vbox
            .ancestor(gtk::Popover::static_type())
            .and_downcast::<gtk::Popover>();
        let close_menu = |b: &gtk::Button, po: Option<gtk::Popover>| {
            b.connect_clicked(move |_| {
                if let Some(p) = &po {
                    p.popdown();
                }
            });
        };
        vbox.append(&menu::sep());

        // 分屏 / 关闭：图标按"并排 / 上下堆叠"选，关终端用危险色。
        let split = |icon: &str, label: &str, stacked: bool, danger: bool| {
            let b = menu::item(icon, label, None);
            if danger {
                menu::danger(&b);
            }
            close_menu(&b, pop_owner.clone());
            let pg = page.clone();
            let pn = pane_menu.clone();
            b.connect_clicked(move |_| {
                // 右键哪块就分/关哪块，不看当前焦点
                if danger {
                    crate::tabs::close_pane(&pg, &pn);
                } else {
                    crate::tabs::split_pane(&pg, &pn, stacked);
                }
            });
            vbox.append(&b);
        };
        split("view-dual-symbolic", "左右分屏", false, false);
        split("view-continuous-symbolic", "上下分屏", true, false);
        split("window-close-symbolic", "关闭当前终端", false, true);

        // 快速命令：往**这个**终端里敲一行预设命令。
        //
        // **为什么挪到这儿**：标签头上的图标按钮按用户要求撤掉了（标签头
        // 只留状态点 + 标题 + ×）。主菜单与 Ctrl+Shift+K 还在，但离终端最远；
        // 终端右键菜单是"终端动作"最顺手的落点（分屏/广播/日志都在这）。
        {
            let b = menu::item(
                "utilities-terminal-symbolic",
                "快速命令…",
                Some("Ctrl+Shift+K"),
            );
            b.set_tooltip_text(Some(
                "挑一条预设命令，直接敲进「这个」终端（不是当前焦点那个）。\n\
                 预设命令在「工具 → 快速命令」里维护。",
            ));
            let pg = page.clone();
            let po = pop_owner.clone();
            let term = pane_menu.terminal().clone();
            b.connect_clicked(move |_| {
                if let Some(p) = &po {
                    p.popdown();
                }
                let Some(st) = crate::tabs::panel_state_for(&pg) else {
                    return;
                };
                // 外层是 Fn 闭包，不能把 term 移走——先克隆一份再交给 idle
                let term = term.clone();
                glib::idle_add_local_once(move || {
                    let t2 = term.clone();
                    crate::panel::on_open_quickcmds(&st, &t2, t2.upcast_ref());
                });
            });
            vbox.append(&b);
        }

        vbox.append(&menu::sep());

        // 3.2 输入广播（勾选标记在右侧，点击整行即开关）
        let cb = menu::toggle(
            "network-transmit-receive-symbolic",
            "按键广播到其它终端",
            Some("Alt+Shift+B"),
            crate::tabs::broadcast_on(),
        );
        cb.set_tooltip_text(Some(
            "打开后，在任意终端里敲的按键会同时发给其它终端（Alt+Shift+B 开关）。\n\
             分屏的两个终端是各自独立的会话，所以也会互相转发——\n\
             不想同步时随时关掉即可。\n\
             目标终端若开了应用光标键（vi/less），方向键仍按 ESC [ A 发送。",
        ));
        {
            let po = pop_owner.clone();
            cb.connect_clicked(move |c| {
                if let Some(p) = &po {
                    p.popdown();
                }
                crate::tabs::set_broadcast(c.is_active());
            });
        }
        vbox.append(&cb);

        // 3.4 终端日志
        let cb = menu::toggle(
            "document-save-symbolic",
            "记录终端到日志文件",
            None,
            inner.log_path().is_some(),
        );
        cb.set_tooltip_text(Some(&format!(
            "把这个终端的屏幕文字逐行写进 {} 下的文件（会话名-时间戳.log）。\n\
             抓的是屏幕而不是原始字节流：同一行被反复改写会记多行，\
             滚过一屏以上或被清屏会漏记。\n\
             文件里是纯文字、不含颜色（要颜色、能回放请用「录制会话到 .cast」）。\n\
             分屏的两个终端各自开关、各自记；互不影响。",
            default_log_dir().display()
        )));
        let pane_log = pane_menu.clone();
        let po_log = pop_owner.clone();
        cb.connect_clicked(move |c| {
            if let Some(p) = &po_log {
                p.popdown();
            }
            if c.is_active() {
                if let Err(e) = pane_log.toggle_log() {
                    crate::panel::dialogs::show_error(None, "无法记录日志", &e);
                }
            } else {
                pane_log.stop_log();
            }
            pane_log.sync_log_ui();
        });
        vbox.append(&cb);

        // 6.1 会话录制
        let cb = menu::toggle(
            "media-record-symbolic",
            "录制会话到 .cast",
            None,
            inner.rec_path().is_some(),
        );
        cb.set_tooltip_text(Some(&rec_menu_tooltip(&default_capture_dir())));
        let pane_rec = pane_menu.clone();
        let po_rec = pop_owner.clone();
        cb.connect_clicked(move |c| {
            if let Some(p) = &po_rec {
                p.popdown();
            }
            if c.is_active() {
                if let Err(e) = pane_rec.toggle_recording() {
                    crate::panel::dialogs::show_error(None, "无法录制会话", &e);
                }
            } else {
                pane_rec.stop_recording();
            }
            pane_rec.sync_rec_ui();
        });
        vbox.append(&cb);

        vbox.append(&menu::sep());

        let b_play = menu::item(
            "media-playback-start-symbolic",
            "打开 .cast 回放…",
            Some("Ctrl+Shift+O"),
        );
        b_play.set_tooltip_text(Some(replay_menu_tooltip()));
        close_menu(&b_play, pop_owner.clone());
        b_play.connect_clicked(|_| crate::replayui::pick_and_open());
        vbox.append(&b_play);

        let b_last = menu::item("document-open-recent-symbolic", "回放最近一次录制", None);
        let has_last = crate::term::latest_capture_name().is_some();
        b_last.set_sensitive(has_last);
        b_last.set_tooltip_text(Some(if has_last {
            "直接打开录制目录里时间最新的那个 .cast"
        } else {
            "录制目录里还没有 .cast 文件"
        }));
        close_menu(&b_last, pop_owner.clone());
        b_last.connect_clicked(|_| {
            if let Err(e) = crate::replayui::replay_latest() {
                crate::term::alert::<gtk::Window>(None, &e);
            }
        });
        vbox.append(&b_last);
    }));
}



/// 终端配色（4.1）。与界面主题各选各的：界面浅色不代表终端也要浅色。
pub fn apply_colors(term: &vte::Terminal) {
    apply_palette(term, crate::theme::palette());
}

fn apply_palette(term: &vte::Terminal, p: crate::theme::Palette) {
    let (fgs, bgs, hex) = p.colors();
    let pal: Vec<gdk::RGBA> = hex.iter().filter_map(|h| gdk::RGBA::parse(*h).ok()).collect();
    let pal_ref: Vec<&gdk::RGBA> = pal.iter().collect();
    let fg = gdk::RGBA::parse(fgs).unwrap();
    let bg = gdk::RGBA::parse(bgs).unwrap();
    term.set_colors(Some(&fg), Some(&bg), &pal_ref);
}

/* ---------------- spawn ---------------- */

fn spawn_term(inner: &Rc<TermInner>) {
    let o = &inner.opts;
    inner.child_alive.set(false);
    let argv: Vec<String> = inner.ssh_argv();

    if argv.is_empty() {
        alert::<gtk::Window>(
            None,
            "找不到可用的客户端程序（ssh/plink/telnet/nc）。\n请安装 openssh-client。",
        );
        return;
    }

    let env = build_env(o);
    autofeed_start(inner);
    inner.spawn_wall.set(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
    );

    let cwd = glib::home_dir().to_string_lossy().into_owned();
    let argv_ref: Vec<&str> = argv.iter().map(String::as_str).collect();
    let env_ref: Vec<&str> = env.iter().map(String::as_str).collect();

    let pty = match vte::Pty::new_sync(vte::PtyFlags::empty(), None::<&gio::Cancellable>) {
        Ok(p) => p,
        Err(e) => {
            alert::<gtk::Window>(None, &format!("创建 PTY 失败：{e}"));
            return;
        }
    };
    inner.term.set_pty(Some(&pty));

    let inner_cb = Rc::clone(inner);
    pty.spawn_async(
        Some(cwd.as_str()),
        &argv_ref,
        &env_ref,
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gio::Cancellable>,
        move |res| match res {
            Ok(pid) => {
                inner_cb.child_alive.set(true);
                inner_cb.term.watch_child(pid);
                // 阶段 65：会话起来了 → 通知上层
                //
                // ⚠️ **只报一次**：`set_alive_hook` 可能已经补报过
                // （构造完才装钩子时），再报一次面板的存活数就翻倍了
                // （两个终端页签时计数对不上）。
                if !inner_cb.alive_reported.get() {
                    inner_cb.alive_reported.set(true);
                    if let Some(h) = inner_cb.alive_hook.borrow().as_ref() {
                        h(true);
                    }
                }
                // 新会话起来了，「会话已结束」条收起来
                inner_cb.banner_re.set_visible(false);
                inner_cb.sync_banner_visibility();
                // 会话就绪钩子（「在此处打开终端」靠它发 `cd`）
                // ⚠️ 先 take 出来再调：闭包里若又碰这个 RefCell 会死锁。
                if let Some(h) = inner_cb.ready_hook.borrow_mut().take() {
                    h(&inner_cb.term);
                }
            }
            Err(e) => {
                // 起不来就把「重新连接」留在那儿，用户可以再点一次
                inner_cb.banner_re.set_visible(true);
                inner_cb.sync_banner_visibility();
                alert::<gtk::Window>(
                    None,
                    &format!(
                        "启动 SSH 客户端失败：{e}\n\n可点终端上方的「重新连接」，或安装 openssh-client / putty。"
                    ),
                );
            }
        },
    );
}

/// GTK4 无 gtk_dialog_run，统一用 AlertDialog（4.10+）。
pub fn alert<W: IsA<gtk::Window>>(parent: Option<&W>, msg: &str) {
    let dlg = gtk::AlertDialog::builder()
        .message("PuTTYPlus")
        .detail(msg)
        .modal(true)
        .build();
    dlg.show(parent);
}

/* ---------------- PTY 自动应答 ----------------
 * putty 携带 -pw 时会在服务器索取密码时自动应答；部分发行版（如 Deepin）
 * 定制过的 ssh 无视 SSH_ASKPASS，只能通过向 PTY 写入来模拟。
 * 仅在明确提供了 -pw 且未使用密钥时启用；限制次数与时长，
 * 避免把密码误发给登录之后的普通程序（如 sudo）。 */

fn autofeed_start(inner: &Rc<TermInner>) {
    *inner.last_line_fed.borrow_mut() = None;
    inner.autofeeds.set(0);
    let o = &inner.opts;
    if o.password_str().map_or(false, |p| !p.is_empty())
        && o.keyfile.as_deref().map_or(true, |k| k.is_empty())
        && o.is_ssh()
    {
        inner.autofeed_on.set(true);
        inner
            .autofeed_deadline
            .set(glib::monotonic_time() as u64 / 1000 + AUTOFEEED_MS);
        let tick = Rc::clone(inner);
        glib::timeout_add_local(std::time::Duration::from_millis(150), move || {
            autofeed_cb(&tick)
        });
    } else {
        inner.autofeed_on.set(false);
    }
}

fn autofeed_cb(inner: &Rc<TermInner>) -> glib::ControlFlow {
    if !inner.autofeed_on.get() {
        return glib::ControlFlow::Break;
    }
    if inner.autofeeds.get() >= AUTOFEEED_MAX
        || (glib::monotonic_time() as u64 / 1000) > inner.autofeed_deadline.get()
    {
        inner.autofeed_on.set(false);
        return glib::ControlFlow::Break;
    }

    let Some(txt) = inner.term.text_format(vte::Format::Text) else {
        return glib::ControlFlow::Continue;
    };
    let lines: Vec<&str> = txt.split('\n').collect();
    let n = lines.len();
    let mut scanned = 0;
    let mut did_feed = false;
    let dbg = std::env::var("PUTTYPLUS_AUTOFEED_DEBUG").is_ok();

    // 从最新往回扫最近几个非空行：认证提示之后可能还追加了
    // 系统组件打印的警告文本（如发行版定制的 askpass 助手）
    let mut i = n as i64 - 1;
    while i >= 0 && scanned < 6 && !did_feed {
        let raw = lines[i as usize];
        i -= 1;
        if raw.is_empty() {
            continue;
        }
        scanned += 1;
        if dbg {
            eprintln!("[autofeed] line: {raw}");
        }
        if Some(raw.to_string()) == *inner.last_line_fed.borrow() {
            break; // 已对这行喂过，等新提示行出现
        }

        let trimmed = raw.trim_end();
        let len = trimmed.len();
        let ends_colon =
            !trimmed.is_empty() && (trimmed.ends_with(':') || trimmed.ends_with('：'));
        let low = trimmed.to_lowercase();

        let pw = inner.opts.password_str().unwrap_or("");
        // i18n:keep
        // ⚠️ 下面认的是**远端终端**的提示文字 —— 那是**别人机器上**的
        // 字符串（可能是中文，也可能是英文），**不是本程序的界面文案**
        // ⇒ 绝对不能翻译（翻了中文那句就再也认不出来了）。
        let want_pw = !pw.is_empty()
            && !trimmed.is_empty()
            && len < 200
            && ((ends_colon && low.contains("password"))
                || trimmed.contains("密码")
                || (ends_colon && low.contains("passphrase"))
                || (ends_colon && low.contains("verification code")));
        // i18n:end
        // plink 的提示写作 "(y/n, Return cancels connection, ...)"
        let want_yes = !trimmed.is_empty()
            && len < 200
            && (low.contains("(y/n")
                || low.contains("yes/no")
                || low.contains("store key")
                || low.contains("continue connecting"));
        // plink 认证成功后等待 "Access granted. Press Return to begin session."
        let want_return =
            !pw.is_empty() && !trimmed.is_empty() && len < 200 && low.contains("press return");

        if want_pw {
            inner.term.feed_child(format!("{pw}\n").as_bytes());
            did_feed = true;
        } else if want_yes {
            inner.term.feed_child(b"y\n");
            did_feed = true;
        } else if want_return {
            inner.term.feed_child(b"\n");
            did_feed = true;
        }
        if did_feed {
            inner.autofeeds.set(inner.autofeeds.get() + 1);
            *inner.last_line_fed.borrow_mut() = Some(raw.to_string());
        }
    }
    glib::ControlFlow::Continue
}


/// 子串查找用：把用户输入按 PCRE2 字面量转义。
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for c in s.chars() {
        if !c.is_alphanumeric() && c != '_' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    /// 右键菜单里的说明文案必须与**真实行为**一致（阶段 94）。
    ///
    /// ⚠️ 用户 2026-11-01 报「录屏的 tooltip 与现在的真实情况显然已经是
    /// 不符合了」——旧文案说"输入是编码后的按键字节"，而我们**根本不录
    /// 输入事件**（明文密码不落盘），写的是 **asciicast v3**。
    #[test]
    fn 录制说明与真实行为一致() {
        // ⚠️ 坏词拼出来：源码里的字面量会被 `lint_tests` 扫到（会把自己扫出来）
        let old_ver: String = ["asciicast ", "v2"].concat();
        let t = super::rec_menu_tooltip(std::path::Path::new("/tmp/captures"));
        assert!(t.contains("asciicast v3"), "写出的是 v3：{t}");
        assert!(!t.contains(&old_ver), "别把读入兼容的 v2 说成写出格式：{t}");
        assert!(t.contains("输入事件不录"), "得说清不录输入：{t}");
        assert!(t.contains("密码"), "得说清文件里没有密码：{t}");
        assert!(t.contains("敲出来"), "得说清命令是逐次敲出来的：{t}");
        assert!(t.contains("/tmp/captures"), "得说清文件存哪：{t}");
    }

    /// 回放项的说明：别再提「录到的按键」（现在压根没录）。
    #[test]
    fn 回放说明与真实行为一致() {
        let kbd: String = ["录到的", "按键"].concat();
        let t = super::replay_menu_tooltip();
        assert!(t.contains("v2 / v3"), "读入兼容 v2 与 v3：{t}");
        assert!(!t.contains(&kbd), "我们不录按键：{t}");
        assert!(t.contains("不会真的把按键发出去"), "得说清回放不发按键：{t}");
    }

    /// 横幅的说明与菜单项那份**说同一件事**（两处文案曾经各说各的）。
    #[test]
    fn 横幅说明与菜单说明一致() {
        let b = super::rec_banner_tooltip();
        assert!(b.contains("asciicast v3"), "写出的是 v3：{b}");
        assert!(b.contains("输入事件不录"), "得说清不录输入：{b}");
        assert!(b.contains("密码"), "得说清文件里没有密码：{b}");
    }

    use super::*;
    use gdk::ModifierType as M;

    fn enc(k: gdk::Key, m: M) -> Option<String> {
        encode_key(k, m).map(|v| String::from_utf8(v).unwrap())
    }

    /// 阶段 88：敲完一行时，那一行该不该**再批量录一遍**。
    ///
    /// ⚠ 这条钉的是"命令在屏幕上出现两遍"这个坑：光标行现在会**逐次重写**
    /// 录下来 ⇒ 敲完回车时它已经显示过了，批量再录一遍就重了。
    /// 但"敲得比采样快"时它从没显示过 ⇒ 必须照旧批量录，否则命令会凭空消失。
    #[test]
    fn 敲完一行的处理有三种() {
        // 从没显示过（敲得比采样快）⇒ 照旧批量录
        assert_eq!(done_line_action(false, "", "ls"), DoneLine::RecordInBatch);
        // 显示过、已是最终态 ⇒ 跳过（否则屏幕上会出现两遍）
        assert_eq!(done_line_action(true, "ls", "ls"), DoneLine::Skip);
        // 显示过、但不是最终态（最后几下没赶上采样）⇒ 先补一次重写再跳过
        assert_eq!(done_line_action(true, "l", "ls"), DoneLine::RewriteThenSkip);
    }

    /// 重写一行的字节要和 asciinema 录到的**一模一样**（`\r` + 清行），
    /// 否则回放端（真 VTE）还原不出来。
    #[test]
    fn 重写一行是回车加清行() {
        assert_eq!(rewrite_line("ls"), "\r\u{1b}[Kls");
    }

    #[test]
    fn 普通字符原样编码() {
        assert_eq!(enc(gdk::Key::a, M::empty()), Some("a".into()));
        assert_eq!(enc(gdk::Key::A, M::empty()), Some("A".into()));
        assert_eq!(enc(gdk::Key::_1, M::empty()), Some("1".into()));
        assert_eq!(enc(gdk::Key::question, M::empty()), Some("?".into()));
    }

    #[test]
    fn 控制键折成控制字符() {
        assert_eq!(enc(gdk::Key::c, M::CONTROL_MASK), Some("\u{3}".into()));
        assert_eq!(enc(gdk::Key::C, M::CONTROL_MASK), Some("\u{3}".into()));
        assert_eq!(enc(gdk::Key::a, M::CONTROL_MASK), Some("\u{1}".into()));
        // Ctrl+[ = ESC、Ctrl+\ = FS、Ctrl+] = GS、Ctrl+^ = RS、Ctrl+_ = US
        assert_eq!(enc(gdk::Key::bracketleft, M::CONTROL_MASK), Some("\u{1b}".into()));
        assert_eq!(enc(gdk::Key::backslash, M::CONTROL_MASK), Some("\u{1c}".into()));
        assert_eq!(enc(gdk::Key::braceright, M::CONTROL_MASK), Some("\u{1d}".into()));
        assert_eq!(enc(gdk::Key::asciicircum, M::CONTROL_MASK), Some("\u{1e}".into()));
        assert_eq!(enc(gdk::Key::underscore, M::CONTROL_MASK), Some("\u{1f}".into()));
        // Ctrl+空格 = NUL，Ctrl+? = DEL
        assert_eq!(enc(gdk::Key::space, M::CONTROL_MASK), Some("\u{0}".into()));
        assert_eq!(enc(gdk::Key::question, M::CONTROL_MASK), Some("\u{7f}".into()));
        // Ctrl 折不动数字：原样发（与 VTE 一致）
        assert_eq!(enc(gdk::Key::_1, M::CONTROL_MASK), Some("1".into()));
    }

    #[test]
    fn alt_前缀_esc() {
        assert_eq!(enc(gdk::Key::b, M::ALT_MASK), Some("\u{1b}b".into()));
        assert_eq!(enc(gdk::Key::b, M::ALT_MASK | M::CONTROL_MASK), Some("\u{1b}\u{2}".into()));
    }

    #[test]
    fn 特殊键转义序列() {
        assert_eq!(enc(gdk::Key::Return, M::empty()), Some("\r".into()));
        assert_eq!(enc(gdk::Key::KP_Enter, M::empty()), Some("\r".into()));
        assert_eq!(enc(gdk::Key::Tab, M::empty()), Some("\t".into()));
        assert_eq!(enc(gdk::Key::BackSpace, M::empty()), Some("\u{7f}".into()));
        assert_eq!(enc(gdk::Key::Escape, M::empty()), Some("\u{1b}".into()));
        // Shift+Tab（GDK 报 ISO_Left_Tab）
        assert_eq!(enc(gdk::Key::ISO_Left_Tab, M::empty()), Some("\u{1b}[Z".into()));
    }

    #[test]
    fn 方向与功能键带修饰键参数() {
        assert_eq!(enc(gdk::Key::Up, M::empty()), Some("\u{1b}[A".into()));
        assert_eq!(enc(gdk::Key::Down, M::empty()), Some("\u{1b}[B".into()));
        assert_eq!(enc(gdk::Key::Right, M::empty()), Some("\u{1b}[C".into()));
        assert_eq!(enc(gdk::Key::Left, M::empty()), Some("\u{1b}[D".into()));
        assert_eq!(enc(gdk::Key::Home, M::empty()), Some("\u{1b}[H".into()));
        assert_eq!(enc(gdk::Key::End, M::empty()), Some("\u{1b}[F".into()));
        // Ctrl+上 = 1+4 = 5；Shift+上 = 1+1 = 2；Alt+上 = 1+2 = 3
        assert_eq!(enc(gdk::Key::Up, M::CONTROL_MASK), Some("\u{1b}[1;5A".into()));
        assert_eq!(enc(gdk::Key::Up, M::SHIFT_MASK), Some("\u{1b}[1;2A".into()));
        assert_eq!(enc(gdk::Key::Up, M::ALT_MASK), Some("\u{1b}[1;3A".into()));
        // ~ 形式：Delete / PageUp
        assert_eq!(enc(gdk::Key::Delete, M::empty()), Some("\u{1b}[3~".into()));
        assert_eq!(enc(gdk::Key::Delete, M::CONTROL_MASK), Some("\u{1b}[3;5~".into()));
        assert_eq!(enc(gdk::Key::Page_Up, M::empty()), Some("\u{1b}[5~".into()));
        assert_eq!(enc(gdk::Key::Page_Down, M::empty()), Some("\u{1b}[6~".into()));
        // F1-F4 是 SS3 形式，带修饰键时统一升成 CSI
        assert_eq!(enc(gdk::Key::F1, M::empty()), Some("\u{1b}OP".into()));
        assert_eq!(enc(gdk::Key::F1, M::SHIFT_MASK), Some("\u{1b}[1;2P".into()));
        assert_eq!(enc(gdk::Key::F5, M::empty()), Some("\u{1b}[15~".into()));
    }

    #[test]
    fn 小键盘方向键等同主键区() {
        // NumLock 关时 GDK 报 KP_Up，用户期待的和 Up 一样
        assert_eq!(enc(gdk::Key::KP_Up, M::empty()), enc(gdk::Key::Up, M::empty()));
        assert_eq!(enc(gdk::Key::KP_End, M::empty()), enc(gdk::Key::End, M::empty()));
        assert_eq!(enc(gdk::Key::KP_Delete, M::empty()), enc(gdk::Key::Delete, M::empty()));
    }

    #[test]
    fn vte_自截获的组合不广播() {
        // 缩放：Ctrl+ / Ctrl+-
        assert_eq!(enc(gdk::Key::plus, M::CONTROL_MASK), None);
        assert_eq!(enc(gdk::Key::minus, M::CONTROL_MASK), None);
        assert_eq!(enc(gdk::Key::_0, M::CONTROL_MASK), None);
        // 复制/粘贴/全选
        assert_eq!(enc(gdk::Key::c, M::CONTROL_MASK | M::SHIFT_MASK), None);
        assert_eq!(enc(gdk::Key::v, M::CONTROL_MASK | M::SHIFT_MASK), None);
        assert_eq!(enc(gdk::Key::a, M::CONTROL_MASK | M::SHIFT_MASK), None);
        // Ctrl+Insert / Shift+Insert
        assert_eq!(enc(gdk::Key::Insert, M::CONTROL_MASK), None);
        assert_eq!(enc(gdk::Key::Insert, M::SHIFT_MASK), None);
        // 同一个键不带这些修饰时照发不误
        assert_eq!(enc(gdk::Key::plus, M::empty()), Some("+".into()));
        assert_eq!(enc(gdk::Key::c, M::empty()), Some("c".into()));
        assert_eq!(enc(gdk::Key::Insert, M::empty()), Some("\u{1b}[2~".into()));
    }

    #[test]
    fn 纯修饰键不产生字节() {
        assert_eq!(enc(gdk::Key::Shift_L, M::SHIFT_MASK), None);
        assert_eq!(enc(gdk::Key::Control_L, M::CONTROL_MASK), None);
        assert_eq!(enc(gdk::Key::Alt_L, M::ALT_MASK), None);
        assert_eq!(enc(gdk::Key::Super_L, M::empty()), None);
    }

    #[test]
    fn 非ascii_按utf8_编码() {
        assert_eq!(enc(gdk::Key::kana_A, M::empty()), Some("ア".into()));
    }
}

#[cfg(test)]
mod log_tests {
    use super::*;

    fn v(x: &[&str]) -> Vec<String> {
        x.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn 没变化就不写() {
        let a = v(&["l1", "l2", "$ "]);
        assert!(new_lines(&a, &a).is_empty());
    }

    #[test]
    fn 末尾新增一行() {
        let prev = v(&["l1", "l2", "文件1"]);
        let cur = v(&["l1", "l2", "文件1", "文件2"]);
        assert_eq!(new_lines(&prev, &cur), v(&["文件2"]));
    }

    #[test]
    fn 正在输入的命令行会原样记下来() {
        let prev = v(&["$ "]);
        let cur = v(&["$ l"]);
        assert_eq!(new_lines(&prev, &cur), v(&["$ l"]));
        let cur2 = v(&["$ ls"]);
        assert_eq!(new_lines(&cur, &cur2), v(&["$ ls"]));
    }

    #[test]
    fn 滚屏_输出插在老提示符前面() {
        // 回车后屏幕滚一行，命令输出插在"新提示符"之前：
        // prev: [l1, l2, $ , ls, $ ]  →  cur: [l2, $ , ls, file1, $ ]
        // 纯"前缀对齐"在这里会全盘失配（l1 掉了、内容又插在中间）
        let prev = v(&["l1", "l2", "$ ", "ls", "$ "]);
        let cur = v(&["l2", "$ ", "ls", "file1", "$ "]);
        assert_eq!(new_lines(&prev, &cur), v(&["file1", "$ "]));
    }

    #[test]
    fn 清屏不刷一屏空行() {
        let prev = v(&["a", "b", "c"]);
        let cur = v(&["", "", "", ""]);
        assert!(new_lines(&prev, &cur).is_empty());
    }

    #[test]
    fn 末尾空白行不算内容() {
        // 屏幕底部永远是空的输入行，不该每轮都写一个空行
        let prev = v(&["a"]);
        let cur = v(&["a", "b", "", ""]);
        assert_eq!(new_lines(&prev, &cur), v(&["b"]));
    }

    #[test]
    fn 完全对不上就整屏记下来() {
        // 极端情况（终端 resize 等）宁可多记，不要静默漏
        let prev = v(&["xxx", "yyy"]);
        let cur = v(&["aaa", "bbb", "ccc"]);
        assert_eq!(new_lines(&prev, &cur), cur);
        assert_eq!(new_lines(&[], &cur), cur);
    }

    #[test]
    fn 文件名片段清洗() {
        assert_eq!(log_stem("root@10.0.0.9:22"), "root@10.0.0.9_22");
        assert_eq!(log_stem("  我的 会话  "), "我的_会话");
        // 路径分隔符不能进文件名
        assert_eq!(log_stem("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(log_stem("a\nb"), "a_b");
        // 空/全非法字符要有兜底名
        assert_eq!(log_stem("   "), "session");
        assert_eq!(log_stem("///"), "session");
        // 超长截断
        assert_eq!(log_stem(&"x".repeat(200)).chars().count(), 48);
    }
}
