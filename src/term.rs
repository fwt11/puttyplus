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

use crate::args::ConnOptions;
use crate::menu;
use crate::platform::pty::PtySession;
use crate::termview::TermView;
use crate::util::find_program_in_path_win;
use crate::i18n::tr;
use crate::trf;

const AUTOFEEED_MAX: i32 = 3;
const AUTOFEEED_MS: u64 = 30_000;

/* ---------------- askpass 模式 ---------------- */

/// 自身可执行文件路径，用于 SSH_ASKPASS 自我调用判定。
///
/// ⚠️ 跨平台版走 `platform::exe`（Linux 仍是 `/proc/self/exe`，
/// Windows 用 `current_exe()`）——**判定语义不变**，见 `platform::exe` 的说明。
fn self_exe_path() -> Option<std::path::PathBuf> {
    crate::platform::exe::self_exe_path().ok()
}

/// askpass 模式：ssh 会以 [提示文本] 为参数调用本程序并读取 stdout。
/// 判定条件：显式 --askpass，或 SSH_ASKPASS 指向自身且带有密码环境变量。
/// 返回 true 表示已处理（进程已在内部退出）。
pub fn run_askpass(args_after_prog: &[String]) -> bool {
    let pw = std::env::var("PUTTYPLUS_PW").ok();
    let ap = std::env::var("SSH_ASKPASS").ok();
    // ⚠️ 阶段 C8：**会话级密码文件**。
    //
    // 本进程还不知道密码时（Linux 上：终端的 ssh 先要密码，而 SFTP 面板
    // 的对话框还没答），ssh 会问 **askpass 助手**（＝本程序的另一个
    // 进程）⇒ 助手拿不到本进程的内存缓存 ⇒ 靠这个文件跟父进程通气：
    // 父进程（SFTP 面板）把密码写进去，助手等它出现。
    let pw_file = std::env::var("PUTTYPLUS_PW_FILE")
        .ok()
        .filter(|s| !s.is_empty());

    let mut is_askpass = args_after_prog.len() == 1 && args_after_prog[0] == "--askpass";
    if !is_askpass {
        // ⚠️ **有文件也算数**：密码还不知道时环境变量里没有 PUTTYPLUS_PW
        //    ⇒ 以前这里判不出来 ⇒ 助手会当成普通程序启动（然后什么都不做）
        let have_cred = pw.as_ref().map_or(false, |p| !p.is_empty()) || pw_file.is_some();
        if have_cred {
            if let Some(ap) = &ap {
                if !ap.is_empty() {
                    // ⚠️ 判定「SSH_ASKPASS 指的就是我自己」：两边都解析符号链接再比
                    // （`platform::exe::same_file`）——软链接调用也算数。
                    if let Some(selfp) = self_exe_path() {
                        if crate::platform::exe::same_file(&selfp, std::path::Path::new(&ap)) {
                            is_askpass = true;
                        }
                    }
                }
            }
        }
    }
    if !is_askpass {
        return false;
    }
    // 答案：优先环境变量里的密码；没有就**等会话文件**（父进程写）。
    let answer = askpass_answer(
        pw.clone(),
        pw_file
            .as_ref()
            .map(|p| std::path::Path::new(p.as_str())),
        std::time::Duration::from_secs(60),
    );
    use std::io::Write;
    let mut out = std::io::stdout();
    let _ = write!(out, "{}\n", answer);
    let _ = out.flush();
    // 与 C 版 _exit 一致：不做任何清理
    std::process::exit(0);
}

/// **askpass 助手的核心**：算出要交给 `ssh` 的答案。
///
/// - `pw_env` = `PUTTYPLUS_PW`（父进程**已经知道**密码时直接给）。
/// - `file` = `PUTTYPLUS_PW_FILE`（**会话级密码文件**：父进程还不知道
///   密码时，助手在这儿等它被写出来）。
///
/// ⚠️ 返回**空串** = 交白卷（`ssh` 认证失败）⇒ 助手**不能干等**。
///   父进程在用户**取消**时会写一个空文件进来 ⇒ 助手立刻拿到空串 ⇒
///   `ssh` 立刻失败，而不是卡住等 60 秒。
pub fn askpass_answer(
    pw_env: Option<String>,
    file: Option<&std::path::Path>,
    wait: std::time::Duration,
) -> String {
    if let Some(pw) = pw_env.filter(|p| !p.is_empty()) {
        return pw;
    }
    if let Some(path) = file {
        if let Some(pw) = crate::pwcache::read_session_file_wait(path, wait) {
            return pw;
        }
    }
    String::new()
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

/// **回环目标要不要绕开 known_hosts**（堡垒机的本地隧道）。
///
/// ⚠️ **为什么必须绕开**（用户 2026-11-11 报「在堡垒机上分屏失败」）：
///    堡垒机在**本地开一个随机端口**，我们连的就是那个端口 ⇒ 目标恒是
///    `127.0.0.1:<随机端口>`。而
///    - OpenSSH 的 known_hosts 是按 **`[host]:port`** 记的；
///    - 堡垒机的隧道**按访问轮换主机密钥**。
///    ⇒ 同一个端口第二次连就是**另一把 key** ⇒ OpenSSH 直接**硬失败**
///    （`REMOTE HOST IDENTIFICATION HAS CHANGED` + `Host key
///    verification failed.`，退出码 255）⇒ **分屏开不出来**。
///    ⇒ `StrictHostKeyChecking=no` **不行**：它照样会刷那句吓人的警告
///    （实测），只是不失败而已。
///    ⇒ 只有「**根本没有 known_hosts 可查**」才干净。
///
/// ⚠️ **与另外两条路保持一致**：
///    - 进程内那条（russh）：`backend::conn::is_loopback` ⇒ 回环直接放行
///      （`conn.rs` 的注释写的就是这个场景）；
///    - C 版：`sftp_backend.c` 里也是回环就跳过校验。
pub fn loopback_host_key_opts(host: &str) -> Vec<&'static str> {
    if crate::backend::conn::is_loopback(host) {
        vec![
            // ⚠️ **两个都要给**：只置 `UserKnownHostsFile` 的话，
            //    `/etc/ssh/ssh_known_hosts`（全局）里若有该主机的条目
            //    仍会被查到 ⇒ 照样可能撞上「key 变了」。
            "UserKnownHostsFile=/dev/null",
            "GlobalKnownHostsFile=/dev/null",
        ]
    } else {
        Vec::new()
    }
}

pub fn build_ssh_argv(o: &ConnOptions) -> Option<Vec<String>> {
    let ssh = find_program_in_path_win("ssh")?;
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
    // ⚠️ **回环目标（堡垒机的本地隧道）把 known_hosts 置空** ⇒ 见
    //    `loopback_host_key_opts` 的注释（不置空 ⇒ 分屏必被
    //    「REMOTE HOST IDENTIFICATION HAS CHANGED」硬失败）。
    for opt in loopback_host_key_opts(&o.host) {
        av.push("-o".into());
        av.push(opt.into());
    }
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
    let plink = find_program_in_path_win("plink")?;
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
    let selfp = crate::platform::exe::self_exe_path().ok()?;
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
            let sercfg = format!("{},{},{},{},{}",
                o.serial_baud, o.serial_data, o.serial_parity, o.serial_stop, o.serial_flow
            );
            Some(vec![selfs, "--bridge-serial".into(), o.host.clone(), sercfg])
        }
        "raw" => {
            let nc = find_program_in_path_win("nc")?;
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
    build_env_with_pw(o, None)
}

/// 同 [`build_env`]，但**可以补一个密码**（阶段 C8：本次运行内问过的）。
///
/// `pw` = 本次运行内已经问过的密码（`crate::pwcache`）。
/// 优先级：**命令行/连接条目里的密码 > 缓存里的**。
pub fn build_env_with_pw(o: &ConnOptions, pw: Option<&str>) -> Vec<String> {
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

    let pw = pw.filter(|p| !p.is_empty()).or_else(|| o.password_str());
    // 有密码、无密钥时启用 askpass 自动应答（OpenSSH >= 8.4）
    //
    // ⚠️ 阶段 C8：**连"还不知道密码"的情况也接上 askpass**。
    //
    // 以前只在"已经有密码"时才设 ⇒ 密码未知时 ssh 就在 **tty 上**问 ⇒
    // 用户在终端里敲一次、SFTP 面板那个对话框再敲一次 ⇒ **两个地方、
    // 两套体验**（而且 Linux 与 Windows 不一样）。
    //
    // ⇒ 现在**一律**把 ssh 的提问交给 askpass 助手（＝本程序）：
    // - 已知密码 ⇒ 助手直接答（`PUTTYPLUS_PW`）；
    // - 未知密码 ⇒ 助手去等**会话级密码文件**（SFTP 面板那个对话框会把
    //   密码写进去）⇒ **两个平台都是「我们的对话框问一次」**。
    if o.is_ssh() && o.keyfile.as_deref().map_or(true, |k| k.is_empty()) {
        if let Some(selfp) = self_exe_path() {
            set("SSH_ASKPASS", selfp.to_string_lossy().into_owned());
            set("SSH_ASKPASS_REQUIRE", "force".into());
            match pw {
                Some(p) => set("PUTTYPLUS_PW", p.to_string()),
                None => {
                    // ⚠️ **密码还不知道** ⇒ 助手去等会话级密码文件
                    let k = crate::pwcache::key(&o.host, o.port, &o.user);
                    set(
                        "PUTTYPLUS_PW_FILE",
                        crate::pwcache::session_file_path(&k)
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
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

/// 会话就绪钩子要发的东西（「在此处打开终端」用）。
///
/// ⚠️ **为什么分两种**：
///
/// - `Cmd` —— 初始命令（`cd <目录>`）。等**看见 shell 提示符**之后发，
///   这时命令落进的是真 shell，不会被任何登录脚本吃掉；
/// - `Probe` —— 只发一个**空行**。堡垒机那类登录脚本会先打印
///   "Access granted. Press Return to begin session." 然后**读掉一整行**
///   才往下走 —— 空行正好当它那次回车；它走完了，shell 的提示符才会
///   出现（才能走到 `Cmd` 那一支）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyCmd {
    /// 发初始命令。
    Cmd,
    /// 只发一个空行（探路）。
    Probe,
}

pub(crate) struct TermInner {
    term: TermView,
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
    /// **会话就绪钩子**：**远程准备好了**之后调一次。
    ///
    /// 「在此处打开终端」要用：新终端连上后往 pty 里写 `cd <目录>`，
    /// 让它进到用户右键的那个目录。
    ///
    /// ⚠️ **时机不是「ssh 子进程起来了」**（见 `feed_when_remote_ready`）：
    /// 那时候 ssh 还没连上、pty 还没切 raw 模式，写进去的字节会被本地
    /// 回显 + 缓冲，然后被远程的**登录流程**吃掉 ⇒ `cd` 根本不执行。
    ///
    /// ⚠️ 钩子拿到的是**终端**，不是 TermPane：闭包一旦捕获 TermPane，
    /// 就是「TermPane → ready_hook → TermPane」的**引用环**，面板永远
    /// 释放不掉（AGENTS.md 的红线）。
    ready_hook: RefCell<Option<Box<dyn Fn(&TermView, ReadyCmd) + 'static>>>,
    /// **重连钩子**（阶段 99）：这条终端**重连**时调一次。
    ///
    /// 干什么用：「重新连接终端」时**顺带把断了的 SFTP 也救回来**——
    /// 终端与 SFTP 是**两条独立的连接**，只救一条的话，另一条还断着
    /// （用户 2026-11-06 报的现象）。
    ///
    /// ⚠️ 闭包**只能抓弱引用**（面板）——抓强引用就是引用环
    /// （TermPane → 钩子 → TermPane ⇒ 面板永远释放不掉）。
    reconnect_hook: RefCell<Option<Box<dyn Fn(i64) + 'static>>>,
    /// **「我现在在哪个远程目录」的询问者**（上层知道）。
    ///
    /// 干什么用：**重连后回到原来的目录**（用户 2026-11-10：「终端短线
    /// 重连后，应该还回到原来的目录」）。终端自己**不知道** cwd ——
    /// OSC 7 只有部分 shell 会发 ⇒ 得问上层。
    ///
    /// ⚠️ 闭包**只能抓弱引用**（面板）——抓强引用就是引用环
    /// （TermPane → 钩子 → TermPane ⇒ 面板永远释放不掉）。
    cwd_provider: RefCell<Option<Box<dyn Fn() -> Option<String>>>>,
    /// **新会话就绪后要 `cd` 回去的目录**（远程绝对路径）。
    ///
    /// 重连前把「我刚才在哪儿」记在这儿 ⇒ 新会话看见提示符后补一条
    /// `cd '<目录>'` ⇒ 用户不用自己再 `cd` 一遍。
    restore_dir: RefCell<Option<String>>,
    /// **断线时用户敲的输入**（阶段 99）：攒着，等重连成功**补发**进新会话。
    ///
    /// 干什么用：会话结束（ssh 子进程退了）之后用户还在敲命令 ⇒ 那几下
    /// 写进了**死掉的 pty**，全丢了 ⇒ 用户以为自己敲过了，白敲。
    /// ⇒ 攒下来，重连后（看见提示符了）补发进去。
    pending_input: RefCell<Vec<u8>>,
    /// **正在重连**（阶段 99）：防止连按几个键就起好几个 ssh。
    reconnecting: Cell<bool>,
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
            if find_program_in_path_win("plink").is_some() {
                build_plink_argv(o).unwrap_or_default()
            } else {
                build_ssh_argv(o).unwrap_or_default()
            }
        } else {
            // telnet/串口：自桥接（子进程 = 本程序，PTY stdio ↔ TCP/串口）
            build_bridge_argv(o).unwrap_or_default()
        }
    }

    /// **会话结束了**（ssh 子进程退了 / 进程内 SSH 的通道关了）。
    ///
    /// ⚠️ **两条路汇到这里**：
    /// - spawn 那条：VTE 的「子进程退出」信号；
    /// - 进程内那条（阶段 B2）：SSH 通道的 `Exit` 事件。
    ///
    /// ⇒ 收尾的活儿（停自动应答、记退出码、摆「会话已结束」条、报
    ///   上层存活数）只有**一份** ⇒ 两边行为必然一致。
    fn on_session_ended(&self, status: i32) {
        self.autofeed_on.set(false);
        self.child_alive.set(false);
        // asciicast v3 的 `x` 事件：会话退出码（回放时看得出是正常退出还是出错退出）
        if let Some(r) = self.rec.borrow_mut().as_mut() {
            r.rec.exit(status as i64);
        }
        self.term.feed(
            tr("\r\n\x1b[1;33m== 会话已结束（状态码见上）。点上方「重新连接」可重建会话 ==\x1b[0m\r\n")
                .as_bytes(),
        );
        // 阶段 25：会话一断就把「重新连接」摆出来。
        // 光在终端里打一行字，多数用户看不出"还能救"。
        self.banner_re.set_visible(true);
        self.sync_banner_visibility();
        // 阶段 65：会话结束了 → 通知上层（标签状态点不再显示绿点）
        //
        // ⚠️ 只在**报过存活**时才报结束：没报过就不用减计数
        //（否则面板的存活数会变成 -1，被 clamp 成 0，
        //   「起过没有」这个事实也就丢了）。
        if self.alive_reported.replace(false) {
            if let Some(h) = self.alive_hook.borrow().as_ref() {
                h(false);
            }
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
            (tr("上一个"), self.inner.search_prev_btn.clone()),
            (tr("下一个"), self.inner.search_next_btn.clone()),
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
        let term = new_term_backend();

        // ⚠️ 阶段 69：终端字号的**全局默认**（首选项中心 →「终端」页）。
        // 优先级：连接里设的字号 > 全局默认 > 终端内置的 10.5。
        // 以前只有前两档 ⇒ 想"把终端字体调大"得逐条连接去改（审计 P1-4）。
        term.set_appearance(
            opts.font_size.or_else(crate::settings::term_font_size),
            opts.bell,
        );
        term.set_mouse_autohide(true);
        term.set_size(100, 32);
        term.set_scrollback_lines(10_000);
        term.apply_colors();

        // 搜索条（默认收起；Ctrl+F 呼出）
        //
        // ⚠️ 用的是**共用构件** `findbar`（阶段 84）：跟编辑器的查找条同一套
        // 外观与按键约定（Enter 下一个 / Shift+Enter 上一个 / Esc 关闭），
        // 两边各画一套的话很快就会打架。
        // 终端这边只要"输入框 + 上一个/下一个"——VTE 的搜索不报匹配数，
        // 也没有"替换"（回滚缓冲改不了）。
        let fb = crate::findbar::build(crate::findbar::FindOpts::minimal());
        let search_entry = fb.find_entry.clone();
        search_entry.set_placeholder_text(Some(tr("在终端中查找…")));
        {
            let term2 = term.clone();
            search_entry.connect_changed(move |e| {
                // ⚠️ 传**普通文本**（不是正则）：转义由后端做
                // （VTE 那边是 PCRE，ghostty 那边是字面量）。
                term2.search_set_pattern(Some(&e.text()));
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
                    e.set_tooltip_text(Some(&trf!("回车：下一个（{v0}\n{v1}：上一个）", v0 = next_hint(), v1 = b.display)));
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
        banner_bc.set_tooltip_text(Some(tr("点击停止广播（Alt+Shift+B）")));
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
        banner_re.set_label(tr("会话已结束 · 点此重新连接"));
        banner_re.set_tooltip_text(Some(
            tr("重新建立这条 SSH 会话（和右键菜单里的「重新连接终端」是同一个动作）"),
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
            reconnect_hook: RefCell::new(None),
            cwd_provider: RefCell::new(None),
            restore_dir: RefCell::new(None),
            pending_input: RefCell::new(Vec::new()),
            reconnecting: Cell::new(false),
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
        term.set_expand(true, true);

        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
        // 分屏后每块面板的最小可用尺寸（再小就没法看终端了）
        box_.set_size_request(160, 100);
        box_.set_hexpand(true);
        box_.set_vexpand(true);
        box_.append(&inner.banner);
        box_.append(&search_bar);
        box_.append(term.widget());

        connect_signals(&inner);
        install_broadcast_capture(&inner);
        // 阶段 99：会话死了 ⇒ 用户敲的输入攒着 + 自动重连一次 + 补发
        install_dead_input_capture(&inner);
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

    pub fn terminal(&self) -> &TermView {
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

    /// 装「**我现在在哪个远程目录**」的询问者（上层知道）。
    ///
    /// 干什么用：**重连后回到原来的目录**。终端自己不知道 cwd（OSC 7
    /// 只有部分 shell 会发）⇒ 重连前问上层要一下，记下来，新会话就绪后
    /// 补一条 `cd` 回去。
    ///
    /// ⚠️ 闭包**只能抓弱引用**（面板）——抓强引用就是引用环。
    pub fn set_cwd_provider<F: Fn() -> Option<String> + 'static>(&self, f: F) {
        *self.inner.cwd_provider.borrow_mut() = Some(Box::new(f));
    }

    /// **指定**新会话就绪后要 `cd` 回去的目录（远程绝对路径）。
    ///
    /// 干什么用：重建终端页签（用户把「终端」子页签 × 掉之后又重建）
    /// —— 新页签自带一条新会话 ⇒ 让它**直接开到原来那个目录**，跟
    /// 「重连」一个效果。
    pub fn set_restore_dir(&self, dir: &str) {
        if dir.is_empty() {
            return;
        }
        *self.inner.restore_dir.borrow_mut() = Some(dir.to_string());
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

    /// 装**重连钩子**（阶段 99）：这条终端重连时调一次。
    ///
    /// 干什么用：「重新连接终端」时**顺带把断了的 SFTP 也救回来**
    /// （终端与 SFTP 是两条独立的连接）。
    ///
    /// ⚠️ 参数 = **新的 ssh 启动时刻**（Unix 秒）：上层的目录跟随轮询
    /// 靠它找远端那个 shell ⇒ 不更新的话它还拿旧时刻去比 ⇒ 找错 shell。
    ///
    /// ⚠️ 闭包**只能抓弱引用**（面板）——抓强引用就是引用环。
    pub fn set_reconnect_hook<F: Fn(i64) + 'static>(&self, f: F) {
        *self.inner.reconnect_hook.borrow_mut() = Some(Box::new(f));
    }

    /// 按当前配色方案重刷本面板（4.1）。切方案时对所有面板调一次。
    pub fn apply_palette(&self) {
        self.inner.term.apply_palette(crate::theme::palette());
    }

    /// 按当前字体设置重刷本面板。
    ///
    /// ⚠️ **字号优先级不变**：会话/连接里设过的字号仍然压过全局默认
    /// （`opts.font_size`）—— 用户在首选项改的是**全局默认**，不该把
    /// 某条连接**特意设过**的字号也改了。
    ///
    /// ⚠️ 改字号会让 VTE **重排**已有内容（行宽变了）⇒ 这就是「实时生效」
    /// 的正解：不是「重开终端才生效」。
    pub fn apply_font(&self) {
        self.inner
            .term
            .set_appearance(self.inner.opts.font_size, self.inner.opts.bell);
    }

    /// 装/卸按键广播钩子。钩子捕获不到 self，按键只以字节形式出去。
    pub(crate) fn set_broadcast_hook(&self, h: Option<BroadcastHook>) {
        *self.inner.key_sink.borrow_mut() = h;
    }

    /// 登记右键菜单的扩展区（分屏/广播/日志）。
    pub fn set_menu_extra(&self, f: MenuExtra) {
        *self.inner.menu_extra.borrow_mut() = Some(f);
    }

    /// **会话就绪钩子**：**远程说完了话**之后调一次（只调一次）。
    ///
    /// 给「在此处打开终端」用——新终端连上后要 `cd` 到用户右键的目录。
    ///
    /// ⚠️ **不是「ssh 子进程起来了」就调**：见 `feed_when_remote_ready`
    /// （写早了会被本地回显、并被远程的登录流程吃掉）。
    ///
    /// ⚠️ 闭包里**别捕获 TermPane 自己**（那是自引用，终端永远释放不掉）。
    /// 钩子收到的是终端本身，往里写字节就行。
    pub fn set_ready_hook<F: Fn(&TermView, ReadyCmd) + 'static>(&self, f: F) {
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
            trf!("⤷ 广播中 · 按键同时发往其它 {targets} 个终端（点击停用）", targets = targets)
        } else {
            tr("⤷ 广播中 · 暂无其它终端可接收（点击停用）").to_string()
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
        eprintln!("{}", trf!("[日志] 写入 {v0} 失败，已停止记录", v0 = log.path.display()));
    }
}

/// 日志轮询：取一次屏幕快照，扣掉上次已记的行，剩下的追加进文件。
fn log_poll(inner: &Rc<TermInner>) -> glib::ControlFlow {
    let snapshot: Vec<String> = inner
        .term
        .visible_text()
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
        std::fs::create_dir_all(&dir).map_err(|e| trf!("创建日志目录失败：{e}", e = e))?;
        let path = dir.join(format!("{}-{}.log",
            log_stem(&session_stem(&self.inner.opts)),
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| trf!("打开日志文件失败：{e}", e = e))?;
        let head = trf!("# PuTTYPlus 终端日志\n# 会话: {v0}\n# 开始: {v1}\n", v0 = session_stem(&self.inner.opts), v1 = chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
        use std::io::Write;
        let _ = file.write_all(head.as_bytes());

        // 先把当前屏幕作为基线记下来：用户往往是"看到出错才想起要开日志"
        let prev: Vec<String> = self
            .inner
            .term
            .visible_text()
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
                self.inner.banner_log.set_text(&trf!("⤷ 记录中 → {p}", p = p));
                self.inner.banner_log.set_tooltip_text(Some(
                    tr("终端屏幕文字逐行落盘。同一行被反复改写会记多行；\
                     两次采样之间滚过一屏以上或被清屏会漏记——抓的是屏幕，不是原始字节流。"),
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
        // **重连前记住「我刚才在哪个目录」** ⇒ 新会话就绪后补一条 `cd`
        // 回去（用户 2026-11-10：「终端短线重连后，应该还回到原来的
        // 目录」）。
        //
        // ⚠️ **问上层要**：终端自己不知道 cwd（OSC 7 只有部分 shell 会发），
        //    面板知道（它用 exec 轮询跟着远端那个 shell）。
        // ⚠️ 问不到就**沿用上次记下的**（面板暂时断了也还能回到老地方）。
        if let Some(dir) = self.current_remote_dir() {
            *self.restore_dir.borrow_mut() = Some(dir);
        }
        spawn_term(self);
        // 阶段 99：告诉上层「这条终端重连了」⇒ 上层顺带把断了的 SFTP 救回来
        //
        // ⚠️ 参数 = **新的 ssh 启动时刻**：上层的目录跟随轮询靠它找远端
        //    那个 shell（`ps` 里按 pts + 启动时间匹配）⇒ 不更新的话它还拿
        //    旧 ssh 的时刻去比 ⇒ 找错 shell ⇒ 面板的目录乱跳。
        //
        // ⚠️ 用 `try_borrow`：钩子里万一又碰到重连（重入）也不会 panic，
        // 顶多少叫一次钩子。
        if let Ok(h) = self.reconnect_hook.try_borrow() {
            if let Some(h) = h.as_ref() {
                h(self.spawn_wall.get());
            }
        }
    }

    /// **这条终端现在在哪个远程目录**（问上层；问不到就用上次记下的）。
    fn current_remote_dir(&self) -> Option<String> {
        let asked = self.cwd_provider.borrow().as_ref().and_then(|f| f());
        asked
            .or_else(|| self.restore_dir.borrow().clone())
            .filter(|d| !d.is_empty())
    }

    /// 这条会话是不是**交互式 shell**（能不能往里发 `cd`）。
    fn interactive_shell(&self) -> bool {
        restore_allowed(self.opts.remote_cmd.as_deref(), self.opts.no_shell)
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
                    trf!("· 已丢 {lost} 行", lost = lost)
                } else {
                    String::new()
                };
                let l = gtk::Label::new(Some(&trf!("● 录制中 → {p}（点此停止）{lost_note}", p = p, lost_note = lost_note)));
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
 *   · 而输入事件里会有**明文密码**：密码输入时远程把 echo 关掉，
 *     那串字节不会回传（我们这边却抓得到）⇒ 落盘就是事故。
 *     asciicast v2 规范也是这么说的（官方 asciinema 默认不录输入，
 *     "实现不应默认录输入，除非用户明确要求"）。
 *   代价：远程关掉回显的场景（vim / less / `su` / 嵌套 ssh）里，
 *   敲的按键不会出现在任何地方——这是可以接受的取舍。
 *
 * 边界（UI 与文档都写明）：
 *   · **正在敲的那一行**（光标行）现在会**逐次重写**（`\r` + 清行 + 行文本），
 *     所以回放里能看到命令是**一个个敲出来的**——和 asciinema 的观感一致；
 *     但**已经输出完的行**被反复改写，仍只留**最后一次**（那不是敲的过程，
 *     是远程在重画，例如进度条）；
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
    pub fn sample(&mut self, term: &TermView, rec: &mut crate::replay::Recorder) -> usize {
        let cur = term.cursor_row();
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
            let text = term.ansi_rows(cur, cur).first().cloned().unwrap_or_default();
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
        let done_text = term.ansi_rows(done_row, done_row)
            .first()
            .cloned()
            .unwrap_or_default();
        match done_line_action(self.cur_shown, &self.cur_text, &done_text) {
            DoneLine::RecordInBatch => {
                // 这行从没显示过（敲得比采样快）⇒ 照旧批量录进去
                let lines = term.ansi_rows(done_row, cur - 1);
                rec.output(&lines);
            }
            DoneLine::Skip => {
                // 显示过、且已是最终态 ⇒ 批量里跳过它；
                // 但**光标得挪到下一行**（回放端的游标还停在那行）
                rec.output_raw("\r\n");
                let lines = term.ansi_rows(done_row + 1, cur - 1);
                rec.output(&lines);
            }
            DoneLine::RewriteThenSkip => {
                // 显示过、但不是最终态 ⇒ 先把最后几下补上（重写一次），
                // 再挪到下一行
                rec.output_raw(&rewrite_line(&done_text));
                rec.output_raw("\r\n");
                let lines = term.ansi_rows(done_row + 1, cur - 1);
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
    let cur = inner.term.cursor_row();
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
            eprintln!("{}", trf!("[录制] 写入 {v0} 失败，已停止记录", v0 = r.rec.path().display()));
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
        std::fs::create_dir_all(&dir).map_err(|e| trf!("创建录制目录失败：{e}", e = e))?;
        let path = dir.join(format!("{}-{}.cast",
            log_stem(&session_stem(&self.inner.opts)),
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        ));
        let cols = self.inner.term.column_count().max(1);
        let rows = self.inner.term.row_count().max(1);
        // 基线 = 开始录制时的光标行号。之后每采样一次，
        // 光标行号涨到哪儿就是录到哪儿（见 `rec_poll`）。
        let prev_row = self.inner.term.cursor_row();
        let rec = crate::replay::Recorder::start(
            &path,
            cols,
            rows,
            &session_stem(&self.inner.opts),
        )
        .map_err(|e| trf!("打开录制文件失败：{e}", e = e))?;
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

/// 在终端上装**断线自愈**的按键捕获（阶段 99）。
///
/// **会话已经死了**（ssh 子进程退了）用户还在敲 ⇒ 那几下写进了**死掉的
/// pty**，全丢了 ⇒ 用户以为敲过了，白敲。
///
/// ⇒ 这里把输入**攒起来**，并**自动重连一次**；等新会话看见提示符了，
/// 把攒下来的输入**补发**进去（继续执行原来的操作）。
///
/// ⚠️ **不吞键**：VTE 照常处理（用户看得见自己敲了什么）——我们只是
/// **另外**留一份副本。
fn install_dead_input_capture(inner: &Rc<TermInner>) {
    let kc = gtk::EventControllerKey::new();
    kc.set_propagation_phase(gtk::PropagationPhase::Capture);
    // 只抓 Weak：controller 挂在终端控件上，抓强的就成环
    let weak = Rc::downgrade(inner);
    kc.connect_key_pressed(move |_k, keyval, _code, mods| {
        let Some(inner) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        // 会话还活着 ⇒ 什么都不用做（输入照常走 VTE）
        if inner.child_alive.get() {
            return glib::Propagation::Proceed;
        }
        // 攒下这次输入（跟 VTE 算的是同一串字节 ⇒ 补发回去就是原样）
        if let Some(bytes) = encode_key(keyval, mods) {
            inner.pending_input.borrow_mut().extend_from_slice(&bytes);
        }
        // **重连一次**（已经在重连就别再起了）
        if !inner.reconnecting.get() {
            inner.reconnecting.set(true);
            inner.reconnect_term();
        }
        glib::Propagation::Proceed
    });
    inner.term.add_controller(kc);
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
            //   · 用户敲的命令**本来就会出现在录制的输出里**——远程 tty 的回显
            //     本身就是输出字节，回放里照样看得到敲了什么；
            //   · 而输入事件里会有**明文密码**：密码输入时远程把 echo 关掉，
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
        term.connect_child_exited(move |status| {
            // ⚠️ 收尾的活儿在 `TermInner::on_session_ended` 里 ⇒
            //    spawn（子进程退出）与进程内 SSH（通道关闭）两条路
            //    **行为完全一致**（阶段 B2）。
            inner.on_session_ended(status);
        });
    }

    // 远程标题（OSC 0/2）→ 回调（标签头/窗口标题）
    {
        let inner = Rc::clone(inner);
        term.connect_title_changed(move |title| {
            if let Some(cb) = inner.title_cb.borrow().as_ref() {
                cb(title);
            }
        });
    }

    // OSC 7 目录跟随
    {
        let inner = Rc::clone(inner);
        term.connect_cwd_changed(move |uri| {
            if let Some(cb) = inner.cwd_cb.borrow().as_ref() {
                cb(uri);
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
 *     远程 vi/less 这类开了应用光标键的程序要按应用光标键才认；
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
    trf!("把这个终端录成 asciicast v3（{v0} 下的 会话名-时间戳.cast）。\n录的是屏幕渲染出来的行（带颜色），不是原始字节流：\n· 你敲的命令会一个个敲出来（光标行逐次重写）；\n· 输入事件不录，所以文件里不会有你敲的密码；\n· 已经输出完的行被反复改写只留最后一次（进度条这类）；\n· 终端被清屏会漏；输出快过采样间隔时可能丢行（横幅上会写丢了多少行）。\n分屏的两个终端各自录制；互不影响。", v0 = dir.display())
}

/// 「打开 .cast 回放…」右键菜单项的说明。
pub fn replay_menu_tooltip() -> &'static str {
    tr("挑一个 asciicast v2 / v3 文件回放（也可以直接用 asciinema play 播）。\n\
     回放只是把录下来的屏幕重放一遍，不会真的把按键发出去。")
}

/// 录制指示条（横幅）的说明。
pub fn rec_banner_tooltip() -> &'static str {
    tr("把会话录成 asciicast v3（.cast），右键终端「打开 .cast 回放」可回放。\n\
     录的是屏幕渲染出来的行（带颜色），不是原始字节流：\n\
     · 你敲的命令会一个个敲出来（光标行逐次重写）；\n\
     · 输入事件不录，所以文件里不会有你敲的密码；\n\
     · 已经输出完的行被反复改写只留最后一次（进度条这类）；\n\
     · 终端被清屏会漏；\n\
     · 输出快过采样间隔、把滚动缓冲挤爆时会丢（横幅上会写丢了多少行）。\n\
     Ctrl+Shift+O 随时打开回放窗口。")
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
    let pop = menu::popover(term.widget());

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
    let b_copy = plain("edit-copy-symbolic", tr("复制"), Some("Ctrl+Shift+C"));
    {
        let w = weak_btns.clone();
        b_copy.connect_clicked(move |_| {
            if let Some(i) = w.upgrade() {
                i.term.copy_selection();
            }
        });
    }
    let b_paste = plain("edit-paste-symbolic", tr("粘贴"), Some("Ctrl+Shift+V"));
    {
        let w = weak_btns.clone();
        b_paste.connect_clicked(move |_| {
            if let Some(i) = w.upgrade() {
                i.term.paste_clipboard();
            }
        });
    }
    vbox.append(&menu::sep());
    let b_re = plain("view-refresh-symbolic", tr("重新连接终端"), None);
    b_re.set_tooltip_text(Some(
        tr("把这个终端的会话断开重连一次（ssh 子进程会被杀掉再起一个）。\n\
         会话已经断了的话，屏幕上那条「会话已结束」也可以直接点。"),
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
            if let Some(t) = g.widget().and_then(|w| TermView::from_widget(&w)) {
                t.paste_clipboard();
            }
        }
    });
    term.add_controller(gesture);
}


/* ---------------- 颜色 ---------------- */


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
        split("view-dual-symbolic", tr("左右分屏"), false, false);
        split("view-continuous-symbolic", tr("上下分屏"), true, false);
        split("window-close-symbolic", tr("关闭当前终端"), false, true);

        // 快速命令：往**这个**终端里敲一行预设命令。
        //
        // **为什么挪到这儿**：标签头上的图标按钮按用户要求撤掉了（标签头
        // 只留状态点 + 标题 + ×）。主菜单与 Ctrl+Shift+K 还在，但离终端最远；
        // 终端右键菜单是"终端动作"最顺手的落点（分屏/广播/日志都在这）。
        {
            let b = menu::item(
                "utilities-terminal-symbolic",
                tr("快速命令…"),
                Some("Ctrl+Shift+K"),
            );
            b.set_tooltip_text(Some(
                tr("挑一条预设命令，直接敲进「这个」终端（不是当前焦点那个）。\n\
                 预设命令在「工具 → 快速命令」里维护。"),
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
                    crate::panel::on_open_quickcmds(&st, &t2, t2.widget());
                });
            });
            vbox.append(&b);
        }

        vbox.append(&menu::sep());

        // 3.2 输入广播（勾选标记在右侧，点击整行即开关）
        let cb = menu::toggle(
            "network-transmit-receive-symbolic",
            tr("按键广播到其它终端"),
            Some("Alt+Shift+B"),
            crate::tabs::broadcast_on(),
        );
        cb.set_tooltip_text(Some(
            tr("打开后，在任意终端里敲的按键会同时发给其它终端（Alt+Shift+B 开关）。\n\
             分屏的两个终端是各自独立的会话，所以也会互相转发——\n\
             不想同步时随时关掉即可。\n\
             目标终端若开了应用光标键（vi/less），方向键仍按 ESC [ A 发送。"),
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
            tr("记录终端到日志文件"),
            None,
            inner.log_path().is_some(),
        );
        cb.set_tooltip_text(Some(&trf!("把这个终端的屏幕文字逐行写进 {v0} 下的文件（会话名-时间戳.log）。\n抓的是屏幕而不是原始字节流：同一行被反复改写会记多行，滚过一屏以上或被清屏会漏记。\n文件里是纯文字、不含颜色（要颜色、能回放请用「录制会话到 .cast」）。\n分屏的两个终端各自开关、各自记；互不影响。", v0 = default_log_dir().display())));
        let pane_log = pane_menu.clone();
        let po_log = pop_owner.clone();
        cb.connect_clicked(move |c| {
            if let Some(p) = &po_log {
                p.popdown();
            }
            if c.is_active() {
                if let Err(e) = pane_log.toggle_log() {
                    crate::panel::dialogs::show_error(None, tr("无法记录日志"), &e);
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
            tr("录制会话到 .cast"),
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
                    crate::panel::dialogs::show_error(None, tr("无法录制会话"), &e);
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
            tr("打开 .cast 回放…"),
            Some("Ctrl+Shift+O"),
        );
        b_play.set_tooltip_text(Some(replay_menu_tooltip()));
        close_menu(&b_play, pop_owner.clone());
        b_play.connect_clicked(|_| crate::replayui::pick_and_open());
        vbox.append(&b_play);

        let b_last = menu::item("document-open-recent-symbolic", tr("回放最近一次录制"), None);
        let has_last = crate::term::latest_capture_name().is_some();
        b_last.set_sensitive(has_last);
        b_last.set_tooltip_text(Some(if has_last {
            tr("直接打开录制目录里时间最新的那个 .cast")
        } else {
            tr("录制目录里还没有 .cast 文件")
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





/* ---------------- spawn ---------------- */

/* ---------------- 「在此处打开终端」：等远程就绪 ---------------- */

/// 采样周期（毫秒）。
const READY_TICK_MS: i64 = 200;
/// 还没看见 shell 提示符 ⇒ 多久后发一个**空行**探路（毫秒）。
const READY_PROBE_MS: i64 = 2500;
/// 探过路之后，屏幕安静多久算「远程说完了」（毫秒）。
const READY_QUIET_MS: i64 = 900;
/// 最多等这么久（远程一声不吭时的兜底）（毫秒）。
const READY_MAX_WAIT_MS: i64 = 8000;

/// 屏幕最后一行像不像 shell 提示符。
///
/// ⚠️ **宁可认不出**（那就走「探路 + 兜底」那一支），也别把欢迎消息里
///    恰好以 `$` 结尾的行当成提示符 —— 认错了就会早发，命令可能被登录
///    脚本吃掉。
///
/// 常见提示符：`$ `（bash）、`# `（root）、`❯ `（fish）、`➜ `（oh-my-zsh）、
/// `λ `（nushell）。**不收 `%` 和 `>`** —— 「磁盘用了 85%」「load > 50」
/// 这种行太常见，容易误判。
pub fn screen_has_prompt(text: &str) -> bool {
    let Some(last) = text.lines().rev().find(|l| !l.trim().is_empty()) else {
        return false;
    };
    let t = last.trim_end();
    let Some(c) = t.chars().last() else {
        return false;
    };
    matches!(c, '$' | '#' | '❯' | '➜' | 'λ')
}

/// **等远程真的就绪了，再把初始命令送进去**（「在此处打开终端」用）。
///
/// ⚠️ **为什么不能「ssh 子进程起来了就写」**：
///
/// pty 不是一个光秃秃的 pipe，它带**行规程**（本地回显 + 规范模式）。
/// ssh 刚 fork 出来、还没把 pty 切到 raw 模式时，写进去的字节会：
///
/// 1. **被本地回显** —— 屏幕上凭空多出一行命令（用户看到的就是
///    `cd '/home/fwt/litellm'` 孤零零地出现在 banner 上面）；
/// 2. **缓冲在本地行规程里** —— 等 ssh 连上后被**转发给远程**。
///    而那会儿远程可能还停在**登录流程**（堡垒机那种
///    "Access granted. Press Return to begin session."），
///    它**读掉一整行**才往下走 ⇒ 这行 `cd` 被它吃掉，**根本没执行**。
///
/// ⇒ 判据：**看见 shell 提示符**（= shell 起来了，命令落进去就一定
/// 执行）**且本地 pty 已切 raw**（= 不会被本地回显）。
///
/// ⚠️ **提示符有时等不到**：堡垒机的提示符卡在登录脚本后面 ——
///    脚本要读一行才往下走，提示符才会出现。所以先发一个**空行探路**
///    （`ReadyCmd::Probe`），喂它那次回车；脚本走完，提示符就出来了。
///
/// ⚠️ 远程一声不吭（没有 MOTD、也没有提示符）时靠 `READY_MAX_WAIT_MS`
///    兜底。
fn feed_when_remote_ready(
    term: &TermView,
    hook: Box<dyn Fn(&TermView, ReadyCmd) + 'static>,
    inner: std::rc::Weak<TermInner>,
    pty: Option<PtySession>,
) {
    let term = term.clone();
    let hook = Rc::new(hook);
    let last_text = Rc::new(RefCell::new(String::new()));
    let last_change = Rc::new(Cell::new(glib::monotonic_time() / 1000));
    let probe_sent = Rc::new(Cell::new(false));
    let start = glib::monotonic_time() / 1000;

    glib::timeout_add_local(
        std::time::Duration::from_millis(READY_TICK_MS as u64),
        move || {
            // 会话已经没了 ⇒ 送进去也没人收，算了吧。
            let Some(i) = inner.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if !i.child_alive.get() {
                return glib::ControlFlow::Break;
            }

            let now = glib::monotonic_time() / 1000;
            let elapsed = now - start;
            let text = term.screen_text(60, 250);
            let changed = {
                let cur = last_text.borrow();
                text != *cur
            };
            if changed {
                *last_text.borrow_mut() = text.clone();
                last_change.set(now);
            }
            // ⚠ **本地 pty 必须已经切到 raw 模式**（ssh 认证通过之后
            // 才切）。没切就写 ⇒ 字节被**本地回显**（屏幕上凭空多一行
            // 命令）并**缓冲**在本地行规程里，等 ssh 切 raw 后才转发给
            // 远程 —— 那时远程可能还停在登录流程里，命令被吃掉。
            // ⚠️ **进程内 SSH 没有本地 pty**（阶段 C7）⇒ 没有本地
            //    行规程 ⇒ 不存在「本地回显 + 缓冲」⇒ 恒为 raw。
            let raw = pty.as_ref().map_or(true, |p| p.is_raw());

            // ① **看见 shell 提示符** ⇒ shell 起来了，命令直接发
            //    （落进的是真 shell，不会被任何登录脚本吃掉）。
            if raw && screen_has_prompt(&text) {
                hook(&term, ReadyCmd::Cmd);
                return glib::ControlFlow::Break;
            }

            if !probe_sent.get() {
                // ② 还没看见提示符 ⇒ 先发一个**空行**探路：堡垒机那种
                //    「Press Return to begin session.」要读掉一整行才往
                //    下走，空行正好当它那次回车；它走完了，提示符才会
                //    出现（下一轮就能走 ①）。
                if (raw && elapsed >= READY_PROBE_MS) || elapsed >= READY_MAX_WAIT_MS {
                    hook(&term, ReadyCmd::Probe);
                    probe_sent.set(true);
                    // 探完之后重新起算「安静」——等远程把话说完。
                    last_change.set(now);
                    return glib::ControlFlow::Continue;
                }
            } else {
                // ③ 探过路了 ⇒ 等远程安静下来（欢迎消息发完了）再发命令。
                //    登录脚本可能读了不止一行，把命令吃掉了 —— 这时命令
                //    还没到 shell，得自己再发一遍。
                let quiet = now - last_change.get() >= READY_QUIET_MS;
                if (quiet && elapsed >= READY_PROBE_MS) || elapsed >= READY_MAX_WAIT_MS {
                    hook(&term, ReadyCmd::Cmd);
                    return glib::ControlFlow::Break;
                }
            }
            glib::ControlFlow::Continue
        },
    );
}

/// **这条会话能不能往里发 `cd`**（纯逻辑，单测直接测它）。
///
/// ⚠️ **远程命令 / `-N` 一律不行**：那种会话里根本没有 shell ⇒ 提示符
///    永远不出现 ⇒ `feed_when_remote_ready` 只能靠超时兜底把命令发出去
///    ⇒ 那行 `cd` 会被**打进正在运行的程序**（比如 vim），把用户的现场
///    搞坏。
fn restore_allowed(remote_cmd: Option<&str>, no_shell: bool) -> bool {
    !no_shell && remote_cmd.is_none_or(|c| c.is_empty())
}

/// **「就绪后发哪个 `cd`」的决策**（纯逻辑，单测直接测它）。
///
/// - `interactive`：交互式 shell 吗（远程命令 / `-N` ⇒ 不是）；
/// - `user_picked`：用户自己挑了目录吗（「在此处打开终端」）；
/// - `dir`：要恢复的目录。
///
/// ⚠️ **用户的显式选择优先**：用户挑了目录 ⇒ **不**发恢复目录的 `cd`
///    （否则恢复目录会把用户刚选的目录覆盖掉）。
fn pick_restore_dir(
    interactive: bool,
    user_picked: bool,
    dir: Option<&str>,
) -> Option<String> {
    if !interactive || user_picked {
        return None;
    }
    let d = dir?;
    if d.is_empty() {
        return None;
    }
    Some(d.to_string())
}

/// 组装「**会话就绪后**」要发的东西的钩子。
///
/// 按顺序发三样：
///
/// 1. **用户自己装的钩子**（「在此处打开终端」：发 `cd <用户选的那个目录>`）；
/// 2. **重连要恢复的目录**（发 `cd '<原来那个目录>'`）；
/// 3. **断线时用户敲的输入**（阶段 99 的补发）。
///
/// ⚠️ **1 与 2 都是 `cd` ⇒ 用户的显式选择优先**：装了用户钩子就**不**再
///    发恢复目录（否则恢复目录会把用户刚选的目录覆盖掉）。
///
/// ⚠️ 2 只在 `ReadyCmd::Cmd`（**看见 shell 提示符**）时发：`Probe` 是
///    探路，那会儿远程还没进 shell，发过去会被登录脚本吃掉。
///
/// ⚠️ 3 也只在 `Cmd` 时发（同理）。
fn build_ready_hook(
    inner: &Rc<TermInner>,
    user_hook: Option<Box<dyn Fn(&TermView, ReadyCmd) + 'static>>,
    pending: Vec<u8>,
) -> Option<Box<dyn Fn(&TermView, ReadyCmd) + 'static>> {
    let picked = pick_restore_dir(
        inner.interactive_shell(),
        user_hook.is_some(),
        inner.restore_dir.borrow().as_deref(),
    );
    let restore = picked.map(|d| crate::tabs::cwd_cmd(&d));
    if pending.is_empty() && user_hook.is_none() && restore.is_none() {
        return None;
    }
    let pending = RefCell::new(Some(pending));
    Some(Box::new(move |term, what| {
        if let Some(h) = &user_hook {
            h(term, what);
        }
        if what != ReadyCmd::Cmd {
            return;
        }
        // 先 cd 回原来的目录（重连后别把用户丢回 home）
        if let Some(cmd) = &restore {
            term.feed_child(cmd.as_bytes());
        }
        // 再补发断线时用户敲的输入
        if let Some(bytes) = pending.borrow_mut().take() {
            if !bytes.is_empty() {
                term.feed_child(&bytes);
            }
        }
    }))
}

/// 新建终端后端。
///
/// ⚠️ **优先级**：环境变量（自测用）→ 设置项（用户选的）→ 平台默认。
///
/// ⚠️ `PP_TERM_BACKEND=ghostty|vte` 可**强制**用某个后端——自测用
/// （`examples/term_reconnect_smoke.rs` 那套冒烟换个环境变量就能跑
/// ghostty 后端）。
fn new_term_backend() -> TermView {
    // ① **环境变量**（自测用，最高优先）：`PP_TERM_BACKEND=ghostty|vte`
    let forced = std::env::var("PP_TERM_BACKEND").unwrap_or_default();
    if forced.eq_ignore_ascii_case("ghostty") {
        #[cfg(feature = "ghostty")]
        {
            return TermView::new_ghostty();
        }
        #[cfg(not(feature = "ghostty"))]
        {
            // ⚠️ 说了要用 ghostty 却没编 ⇒ 说清楚，别静默换一个后端
            eprintln!("PP_TERM_BACKEND=ghostty 但本次构建没有 ghostty 后端");
        }
    }
    if forced.eq_ignore_ascii_case("vte") {
        // ⚠️ **VTE 只有 Linux 有**（Windows 上 `vte4-sys` 编不过）。
        #[cfg(target_os = "linux")]
        {
            return TermView::new();
        }
        #[cfg(not(target_os = "linux"))]
        {
            eprintln!("PP_TERM_BACKEND=vte 但本平台没有 VTE（只有 ghostty）");
        }
    }

    // ② **设置项**（2026-11-10，用户拍板）：两个后端都编进了同一个二进制
    //    ⇒ 用户能在「首选项中心 → 终端」里选用 libvte 还是 ghostty
    //    ⇒ 跨平台开发时不用改环境变量就能跑另一条渲染路径。
    //
    // ⚠️ **只影响新开的终端**：已经开着的终端是建好后就不换后端的。
    if crate::settings::effective_term_backend() == crate::settings::BACKEND_GHOSTTY {
        #[cfg(feature = "ghostty")]
        {
            return TermView::new_ghostty();
        }
        #[cfg(not(feature = "ghostty"))]
        {
            eprintln!("设置里选了 ghostty 后端，但本次构建没有编进去");
        }
    }

    // ③ 平台默认（Linux = VTE，其它 = ghostty）
    TermView::new()
}

/// 终端会话走哪条路（阶段 C7）。
///
/// - `spawn`：起 `ssh` 子进程 + 本地 pty（**Linux 主线**，行为零变化）。
/// - `inproc`：**进程内实现 SSH 客户端**（russh 直接驱动终端内核）。
///
/// ⚠️ **Windows 的默认（2026-11-10，用户拍板）**：**有外部客户端
///    （`ssh.exe` / `plink.exe`）就 spawn，一个都没有才走进程内**。
///
/// - 有客户端 ⇒ 走 spawn ⇒ 终端里跑的是**用户自己的 ssh** ⇒
///   `~/.ssh/config`、agent、ProxyJump、known_hosts 的语义**跟 Linux 版
///   一样**（进程内那条路做不到这些 ⇒ 那才是差异）；
/// - 没有客户端 ⇒ spawn 必然失败（用户 2026-11-08 报的「找不到可用的
///   客户端程序」）⇒ 兜底走**进程内**（Windows 默认没装 OpenSSH，
///   很多机器根本没装）。
///
/// ⚠️ **Linux 主线仍然是 spawn**（AGENTS.md 的零回归红线）。
///
/// ⚠️ `inproc` **只有 ghostty 后端能用**（VTE 的输入必须过一个 pty）
/// ⇒ Linux 上要自测这条通路，得配 `PP_TERM_BACKEND=ghostty`。
///
/// 环境变量 `PP_TERM_SSH=inproc|spawn` 可强制切换（自测用）。
///
/// ⚠️ **别改成私有**：端口转发要靠它决定"转发由谁承接"（见
///    `panel::PanelState` 里 `worker_forwards` 那段）——终端走 spawn 时
///    转发进 ssh 的 argv，终端走 inproc 时转发由程序内的转发引擎承接。
///    两边必须**用同一个判断**，否则会出现"终端没 spawn ssh、转发却发给了
///    ssh"的空转（Windows 装了 OpenSSH 时必现）。
pub fn use_inproc_ssh() -> bool {
    match std::env::var("PP_TERM_SSH")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "inproc" | "1" | "on" => true,
        "spawn" | "0" | "off" => false,
        _ => default_inproc_ssh(),
    }
}

/// **默认走哪条路**（不带环境变量时）。
///
/// ⚠️ 「系统里有没有客户端」要**认 `.exe`**（Windows 上是 `ssh.exe` /
///    `plink.exe`）⇒ 用 [`crate::util::find_program_in_path_win`]。
///
/// ⚠️ **ssh 与 plink 都要算**：终端真正 spawn 出去的是哪个客户端，由
///    `TermInner::ssh_argv` 决定，而那儿是「**装了 putty 优先用 plink**」
///    （C 版同款规则，从首个发布版就有）⇒ 判定必须跟它**一致**，否则会
///    出现「判定说有客户端、实际拼不出 argv」的空转。
pub fn default_inproc_ssh() -> bool {
    decide_term_ssh(
        crate::settings::term_ssh().as_deref(),
        backend_supports_inproc(),
    ) == TermSsh::Shared
}

/// **终端走哪条路**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TermSsh {
    /// spawn 用户自己的 ssh/plink（沿用 `~/.ssh/config`、agent、ProxyJump…）
    Spawn,
    /// **并入共享连接**：终端在连接中枢那条 SSH 连接上开自己的通道
    Shared,
}

/// **算终端该走哪条路**（纯逻辑，好单测）。
///
/// ⚠️ **两个输入**：
/// - `setting`：用户选的（`shared` / `spawn` / 没设 ⇒ `None`）；
/// - `backend_ok`：**当前终端后端能不能用进程内 SSH**（ghostty 可以；
///   VTE 的键盘输入必须过本地 pty ⇒ 阶段 B2 落地之前不行）。
///
/// ⚠️ **默认 = 并入共享连接**（用户 2026-11-11 拍板：「对每个服务器的访问
///    只需要一个连接」）⇒ 终端也得上这条连接，否则「一个连接」做不到。
///
/// ⚠️ **后端不支持 ⇒ 退回 spawn**（不是报错）：用户要的是「能用」⇒
///    宁可退回，也别开不出终端。
pub fn decide_term_ssh(setting: Option<&str>, backend_ok: bool) -> TermSsh {
    match setting.map(str::trim) {
        Some(crate::settings::TERM_SSH_SPAWN) => TermSsh::Spawn,
        _ => {
            if backend_ok {
                TermSsh::Shared
            } else {
                TermSsh::Spawn
            }
        }
    }
}

/// **当前终端后端能不能用进程内 SSH**（= 能不能并入共享连接）。
///
/// ⚠️ **VTE 还不行**：VTE 的键盘输入必须过一个本地 pty ⇒ 阶段 B2
///    （假 pty 接入）落地之前，Linux 默认后端只能用 spawn。
pub fn backend_supports_inproc() -> bool {
    match crate::settings::effective_term_backend() {
        crate::settings::BACKEND_GHOSTTY => cfg!(feature = "ghostty"),
        // ⚠️ 阶段 B2 落地后这里改成 true。
        crate::settings::BACKEND_VTE => false,
        _ => false,
    }
}

/// 终端所在的最上层窗口（给弹窗当父窗口）。
///
/// ⚠️ **为什么要有**：GTK 的弹窗没有父窗口时会警告
///    `GtkDialog mapped without a transient parent`（Linux 上也有，
///    Windows 上更显眼）⇒ 顺手把它消掉。
///
/// ⚠️ 终端控件可能还没挂到窗口上（`root()` 返回 None）⇒ 那就当没有
///    父窗口（退回原样，不报错）。
fn toplevel(term: &TermView) -> Option<gtk::Window> {
    term.widget()
        .root()
        .and_then(|r| r.downcast::<gtk::Window>().ok())
}

/// **进程内 SSH** 的终端会话（阶段 C7：Windows 上不再 spawn `ssh.exe`）。
///
/// 干什么用：Windows 默认没装 `ssh.exe`、`plink` 又不想依赖 ⇒ 终端改成
/// **进程内实现 SSH 客户端**（`backend::shell`）⇒ 不 spawn 任何外部程序，
/// 也就没有「双回显」「本地行规程」「等 ssh 切 raw」那一套。
///
/// ## 线程模型
///
/// - **后台线程**（`pp-ssh-shell`）：一个 tokio 运行时 ⇒ 连上 ⇒ 开通道
///   ⇒ **读循环**（远程字节 → 事件通道 → 主线程）。
/// - **主线程**（GTK）：把事件喂给终端内核 + 收键盘输入（写进通道）。
///
/// ⚠️ **tokio 运行时必须活到会话结束**：`ShellChannel::send`（键盘）与
/// `resize`（窗口尺寸）都要 `rt.spawn` ⇒ 运行时一掉，键盘就没反应了
/// ⇒ 后台线程 `block_on` 到「会话结束」信号才退出。
///
/// ⚠️ **认证失败要问密码**：没有凭据且像是认证失败 ⇒ 弹框问一次再重试
/// （与 SFTP 面板同一套逻辑，见 `panel/mod.rs` 的 `Connected(Err)`）。
fn spawn_term_inproc(inner: &Rc<TermInner>, password: Option<String>) {
    spawn_term_inproc_ex(inner, password, true)
}

/// 进程内 SSH 的终端会话（阶段 C8：带 `allow_cache` 的版本）。
///
/// `allow_cache` = 这次连接能不能用「本次运行内已经问过的密码」
/// （`crate::pwcache`）。
///
/// ⚠️ **必须能把它关掉**：密码**可能已经变了**（用户改完密码，缓存里
///   还是旧的）⇒ 拿旧密码一直重试就是死循环 ⇒ 用缓存重试过的那一次
///   必须**不再用缓存**（要么弹框问新的，要么直接报错）。
fn spawn_term_inproc_ex(
    inner: &Rc<TermInner>,
    password: Option<String>,
    allow_cache: bool,
) {
    let o = inner.opts.clone();
    // ⚠️ **主线程与后台线程都要用这些值** ⇒ 各自拿一份拷贝
    //    （ConnOptions 整体 clone 一次给后台线程；下面几个标量给主线程）。
    let o_bg = o.clone();
    let user = o.user.clone();
    let host = o.host.clone();
    let port = o.port;
    // ⚠️ **阶段 C8：同一条会话的密码只问一次**。
    //    一个会话里 SFTP 面板 / 终端 / 分屏是**三条独立连接** ⇒ 谁先连上
    //    谁问密码，后面的直接用缓存（用户 2026-11-08 报的就是"要分别输
    //    密码，分屏还要再输一次"）。
    //    优先级：**显式给的密码**（用户刚在框里输了 / 命令行给的）
    //    > **缓存里的**（本次运行内问过的）。
    let explicit_pw = password
        .clone()
        .or_else(|| o.password.as_ref().map(|p| p.to_string()))
        .filter(|p| !p.is_empty());
    let session_key = crate::pwcache::key(&host, port, &user);
    let cached_pw = if explicit_pw.is_none() && allow_cache {
        crate::pwcache::get(&session_key).map(|z| z.to_string())
    } else {
        None
    };
    // 用的到底是"显式密码"还是"缓存里的旧密码"？缓存里的可能已经过期
    // ⇒ 认证失败时**要重新问**，不能拿旧的死磕。
    let pw_from_cache = explicit_pw.is_none() && cached_pw.is_some();
    let effective_pw = explicit_pw.clone().or(cached_pw);
    let pw_bg = effective_pw.clone();
    // 「有没有凭据」在主线程判断（要不要弹密码框）
    let have_pw = effective_pw.is_some();
    let have_key = o.keyfile.as_deref().map_or(false, |k| !k.is_empty());
    let cols = inner.term.column_count().max(1) as u16;
    let rows = inner.term.row_count().max(1) as u16;
    // 远程命令（-m / 位置参数）⇒ 走 exec，否则走交互 shell
    let remote_cmd = o.remote_cmd.clone();
    // `-N`（不要 shell）：连上但不请求 shell（终端空白，这是它的语义）
    let no_shell = o.no_shell;
    // `-T`（不要 pty）：远程不分配终端
    let want_pty = !o.no_pty;

    // 事件通道：后台线程 → 主线程（远程字节 + 退出）
    let (ev_tx, ev_rx) =
        async_channel::unbounded::<crate::backend::shell::ShellEvent>();
    // 连接结果：后台线程 → 主线程
    let (res_tx, res_rx) = async_channel::bounded::<
        Result<crate::backend::shell::ShellChannel, String>,
    >(1);

    // ---- 主线程：等连接结果 ----
    let inner_cb = Rc::clone(inner);
    glib::MainContext::default().spawn_local(async move {
        match res_rx.recv().await {
            Ok(Ok(chan)) => {
                // ⚠️ **退出回调**（阶段 B2）：VTE 后端没有「子进程退出」
                //    信号 ⇒ 进程内 SSH 的通道关闭时，由这条通路自己报。
                let inner_exit = Rc::clone(&inner_cb);
                if !inner_cb.term.attach_shell(
                    chan,
                    ev_rx,
                    Box::new(move |code| inner_exit.on_session_ended(code)),
                ) {
                    alert::<gtk::Window>(
                        None,
                        tr("当前终端后端不支持进程内 SSH。"),
                    );
                    return;
                }
                // 会话活着（与 spawn 那条路同一套标志）
                inner_cb.child_alive.set(true);
                if !inner_cb.alive_reported.get() {
                    inner_cb.alive_reported.set(true);
                    if let Some(h) = inner_cb.alive_hook.borrow().as_ref() {
                        h(true);
                    }
                }
                inner_cb.banner_re.set_visible(false);
                inner_cb.sync_banner_visibility();
                // 会话就绪钩子（「在此处打开终端」靠它发 `cd`）
                // ⚠️ **没有 pty** ⇒ 没有本地行规程 ⇒ 不必等 raw。
                inner_cb.reconnecting.set(false);
                let pending = std::mem::take(&mut *inner_cb.pending_input.borrow_mut());
                let user_hook = inner_cb.ready_hook.borrow_mut().take();
                if let Some(hook) = build_ready_hook(&inner_cb, user_hook, pending) {
                    feed_when_remote_ready(
                        &inner_cb.term,
                        hook,
                        Rc::downgrade(&inner_cb),
                        None,
                    );
                }
            }
            Ok(Err(msg)) => {
                inner_cb.reconnecting.set(false);
                inner_cb.banner_re.set_visible(true);
                inner_cb.sync_banner_visibility();
                // ⚠️ **没有凭据且像是认证失败 ⇒ 问一次密码再重试**
                //    （与 SFTP 面板同一套逻辑）。
                //
                // ⚠️ **阶段 C8：缓存里的密码可能是旧的**（用户改过密码）
                //    ⇒ 用它重试**只试一次**（`pw_from_cache`）⇒ 不行就
                //    重新问人，别拿旧密码死磕。
                let need_ask = !have_key
                    && crate::i18n::looks_like_auth_failure(&msg)
                    && (!have_pw || pw_from_cache);
                if need_ask {
                    let detail = trf!(
                        "SSH 连接 {v0}@{v1}:{v2} 需要密码：\n{msg}",
                        v0 = user,
                        v1 = host,
                        v2 = port,
                        msg = msg
                    );
                    let inner_retry = Rc::clone(&inner_cb);
                    let parent = toplevel(&inner_retry.term);
                    // ⚠️ **同一条会话只弹一个框**（SFTP 面板 / 终端 /
                    //    分屏是三条连接，认证各自独立 ⇒ 会同时要密码）。
                    let key = crate::pwcache::key(&host, port, &user);
                    crate::pwcache::ask_once(
                        key.clone(),
                        move |pw| match pw {
                            Some(pw) => {
                                // ⚠️ 阶段 C8：写进会话级密码文件 ⇒ 同会话的
                                //    其它连接（spawn 出去的 ssh 那边）也能用
                                //    这一份 ⇒ **两个平台都是「问一次」**。
                                let _ = crate::pwcache::write_session_file(&key, &pw);
                                // ⚠️ **不再用缓存**（`allow_cache=false`）：
                                //    这次问来的是最新的，缓存里那份可能已经
                                //    过期了（用户改过密码）。
                                spawn_term_inproc_ex(&inner_retry, Some(pw), false);
                            }
                            None => {
                                // 取消 ⇒ 也写一次（空）⇒ 正在等文件的助手
                                // 立刻收到答复，不用干等。
                                let _ = crate::pwcache::write_session_file(&key, "");
                                let parent = toplevel(&inner_retry.term);
                                alert::<gtk::Window>(parent.as_ref(), &tr("未连接"));
                            }
                        },
                        move |done| {
                            crate::panel::dialogs::ask_password(
                                parent.as_ref(),
                                tr("SSH 密码"),
                                &detail,
                                move |pw| done(pw),
                            );
                        },
                    );
                    return;
                }
                let parent = toplevel(&inner_cb.term);
                alert::<gtk::Window>(
                    parent.as_ref(),
                    &trf!("SSH 连接失败：{msg}", msg = msg),
                );
            }
            Err(_) => {}
        }
    });

    // ---- 后台线程：连接 + 读循环 ----
    std::thread::Builder::new()
        .name("pp-ssh-shell".into())
        .spawn(move || {
            let o = o_bg;
            // ⚠️ **用全局运行时**（连接中枢在那儿）：终端与文件面板、
            //    数据库工作台要共用**同一条** SSH 连接 ⇒ russh 的 Handle
            //    必须绑在同一个 runtime 上（Handle 跨 runtime 用不了）。
            let rt = crate::backend::hub::runtime();
            rt.block_on(async move {
                // ⚠️ **阶段 C8**：用**算好的那个**密码（显式 > 缓存）。
                let pw = pw_bg.as_deref().filter(|p| !p.is_empty());
                // ⚠️ **共享连接**：同一台服务器（host+port+user）在整个程序里
                //    只有一条 SSH 连接 ⇒ 终端与文件面板、数据库工作台、
                //    端口映射共用它 ⇒ **密码只问一次**。
                let creds = crate::backend::hub::Creds {
                    password: pw.map(str::to_string),
                    keyfile: o.keyfile.clone().filter(|k| !k.is_empty()),
                    proxycmd: o.proxycmd.clone().filter(|c| !c.is_empty()),
                    keepalive_sec: o.keepalive_sec,
                    auth_method: o.auth_method,
                    auth_order: o.auth_order,
                };
                let skey = crate::backend::hub::ServerKey::new(&o.host, o.port, &o.user);
                let shared = match crate::backend::hub::acquire(skey, &creds).await {
                    Ok(s) => s,
                    Err(e) => {
                        let _ = res_tx.send_blocking(Err(e.0));
                        return;
                    }
                };
                // ⚠️ **端口映射挂在这条共享连接上**（与文件面板同一份规则、
                //    同一个来源 ⇒ 两边算出同一份时不会互相重启）。
                if o.is_ssh() {
                    let rules = crate::backend::forward::inproc_rules(
                        true,
                        true,
                        &o.forwards,
                        &o.inproc_forwards,
                    );
                    let _ = shared
                        .apply_forward_rules(&crate::backend::conn::fwd_source(&o), rules)
                        .await;
                }
                // ⚠️ **shared 必须活到会话结束**：它是这条共享连接的持有者 ⇒
                //    掉了就等于「最后一个使用者放手」⇒ 连接被断开。
                let _shared_guard = shared;
                let session = _shared_guard.session().clone();
                let opened = if no_shell {
                    crate::backend::shell::ShellChannel::open_bare(session.as_ref()).await
                } else {
                    match remote_cmd.as_deref().filter(|c| !c.is_empty()) {
                        Some(cmd) => {
                            crate::backend::shell::ShellChannel::open_exec(
                                session.as_ref(), cmd, cols, rows, want_pty,
                            )
                            .await
                        }
                        None => {
                            crate::backend::shell::ShellChannel::open_shell(
                                session.as_ref(), cols, rows, want_pty,
                            )
                            .await
                        }
                    }
                };
                let chan = match opened {
                    Ok(c) => c,
                    Err(e) => {
                        let _ = res_tx.send_blocking(Err(e.0));
                        return;
                    }
                };
                let _ = res_tx.send_blocking(Ok(chan.clone()));

                // 读循环：远程字节 → 主线程
                let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
                let tx = ev_tx.clone();
                // ⚠️ 用 `Handle::current()` 而不是 `rt.spawn`：async 块
                //    是 `move` 的，捕获 `rt` 会和 `rt.block_on(..)` 的
                //    借用冲突 ⇒ 这里在运行时上下文里拿句柄。
                tokio::runtime::Handle::current().spawn(async move {
                    while let Some(ev) = chan.next_event().await {
                        if tx.send(ev).await.is_err() {
                            break;
                        }
                    }
                    let _ = done_tx.send(());
                });
                // ⚠️ **运行时活到会话结束**（键盘输入/尺寸变化都要 rt.spawn）
                let _ = done_rx.await;
            });
        })
        .ok();
}

fn spawn_term(inner: &Rc<TermInner>) {
    let o = &inner.opts;
    inner.child_alive.set(false);

    // ⚠️ **Windows 上默认走 spawn**（2026-11-10，用户拍板）：**系统里有
    //    外部客户端（`ssh.exe` / `plink.exe`）就 spawn** ⇒ 终端里跑的是
    //    用户自己的 ssh ⇒ `~/.ssh/config` / agent / ProxyJump 的语义跟
    //    Linux 版一样。
    //    **一个都没有**（Windows 默认没装 OpenSSH）⇒ spawn 必然失败
    //    （用户 2026-11-08 报的「找不到可用的客户端程序」）⇒ 兜底走
    //    **进程内实现 SSH 客户端**。
    //    ⚠️ **Linux 主线仍然是 spawn**（零回归红线）。
    if o.is_ssh() && use_inproc_ssh() {
        spawn_term_inproc(inner, None);
        return;
    }

    let argv: Vec<String> = inner.ssh_argv();

    if argv.is_empty() {
        alert::<gtk::Window>(
            None,
            tr("找不到可用的客户端程序（ssh/plink/telnet/nc）。\n请安装 openssh-client。"),
        );
        return;
    }

    let env = build_env_with_pw(
        o,
        // ⚠️ 阶段 C8：**本次运行内问过的密码**（同一会话的 SFTP 面板
        //    问过了 ⇒ 这条终端不用再问一遍 ⇒ 两个平台一致）。
        crate::pwcache::get(&crate::pwcache::key(&o.host, o.port, &o.user))
            .map(|z| z.to_string())
            .as_deref(),
    );
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

    let pty = match PtySession::new() {
        Ok(p) => p,
        Err(e) => {
            alert::<gtk::Window>(None, &trf!("创建 PTY 失败：{e}", e = e));
            return;
        }
    };
    inner.term.attach_pty(&pty);

    let inner_cb = Rc::clone(inner);
    // 克隆一份 pty 给就绪钩子：要读它的 termios 判断 ssh 有没有把本地
    // tty 切到 raw（VTE 自己那份随后随 `term.set_pty` 一起活着）。
    let pty_cb = pty.clone();
    pty.spawn_async(
        cwd.as_str(),
        &argv_ref,
        &env_ref,
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
                //
                // ⚠️ **不是「子进程起来了」就发**：那时 ssh 还没连上，
                //    pty 还没切 raw 模式 ⇒ 写进去的字节被本地回显（屏幕上
                //    凭空多一行命令）并缓冲在本地行规程里，等 ssh 连上后
                //    转发给远程 —— 那会儿远程可能还停在**登录流程**
                //    （堡垒机常见），这行就被它吃掉，`cd` 根本没执行。
                //
                // ⚠️ **阶段 99**：一起把「断线时用户敲的输入」带上 ——
                //    看见提示符后**补发**进去（继续执行原来的操作）。
                inner_cb.reconnecting.set(false);
                let pending = std::mem::take(&mut *inner_cb.pending_input.borrow_mut());
                let user_hook = inner_cb.ready_hook.borrow_mut().take();
                if let Some(hook) = build_ready_hook(&inner_cb, user_hook, pending) {
                    feed_when_remote_ready(
                        &inner_cb.term,
                        hook,
                        Rc::downgrade(&inner_cb),
                        Some(pty_cb),
                    );
                }
            }
            Err(e) => {
                // 起不来就把「重新连接」留在那儿，用户可以再点一次
                // ⚠️ 阶段 99：起失败 ⇒ 别把"正在重连"卡住（不然用户再敲键
                // 也不会再试一次了）
                inner_cb.reconnecting.set(false);
                inner_cb.banner_re.set_visible(true);
                inner_cb.sync_banner_visibility();
                alert::<gtk::Window>(
                    None,
                    &trf!("启动 SSH 客户端失败：{e}\n\n可点终端上方的「重新连接」，或安装 openssh-client / putty。", e = e),
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

    let Some(txt) = inner.term.visible_text() else {
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
        // ⚠️ 下面认的是**远程终端**的提示文字 —— 那是**别人机器上**的
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
    /// **回环目标（堡垒机的本地隧道）必须绕开 known_hosts**。
    ///
    /// ⚠️ **为什么这条是硬要求**（用户 2026-11-11 报「在堡垒机上分屏失败」）：
    ///    堡垒机本地开随机端口 + 按访问轮换主机密钥 ⇒ OpenSSH 按
    ///    `[host]:port` 记的 known_hosts **必然对不上** ⇒ 硬失败。
    ///
    /// ⚠️ **断言要选「与实现无关」的外部量**：这里断言**要传给 ssh 的
    ///    `-o` 选项**（那才是 OpenSSH 真正看到的东西）⇒ 改回旧行为
    ///    （不置空 known_hosts）时立刻变红。
    #[test]
    fn 回环目标绕开known_hosts() {
        use super::loopback_host_key_opts;
        // 堡垒机的本地隧道 ⇒ 恒是回环
        for h in ["127.0.0.1", "127.0.0.2", "localhost", "::1"] {
            let opts = loopback_host_key_opts(h);
            assert!(
                opts.contains(&"UserKnownHostsFile=/dev/null"),
                "{h} ⇒ 没把 UserKnownHostsFile 置空 ⇒ 分屏会被「key 变了」硬失败"
            );
            assert!(
                opts.contains(&"GlobalKnownHostsFile=/dev/null"),
                "{h} ⇒ 没把 GlobalKnownHostsFile 置空 ⇒ 全局 known_hosts 里若有条目仍会撞上"
            );
        }
        // 普通主机 ⇒ **不动** known_hosts（主机密钥校验是 valuable 的）
        for h in ["example.com", "10.0.0.9", "bastion.corp"] {
            assert!(
                loopback_host_key_opts(h).is_empty(),
                "{h} 不是回环 ⇒ 不该绕开主机密钥校验"
            );
        }
    }

    /// **普通主机的 spawn argv 不能变**（对外契约）。
    ///
    /// ⚠️ 回环目标多两条 `-o` 是**有意差异**（见 `loopback_host_key_opts`），
    ///    但**普通主机必须一个字节都不差** ⇒ 堡垒机那边调用方看到的
    ///    ssh argv 跟以前一样。
    #[test]
    fn 普通主机的argv不受影响() {
        use crate::args::ConnOptions;
        use super::build_ssh_argv;
        let o = ConnOptions {
            proto: "ssh".into(),
            host: "example.com".into(),
            port: 22,
            user: "ops".into(),
            ..Default::default()
        };
        let Some(av) = build_ssh_argv(&o) else {
            // 机器上没有 ssh ⇒ 这条测不了，跳过（不算失败）。
            return;
        };
        assert!(
            !av.iter().any(|a| a.contains("KnownHostsFile")),
            "普通主机的 argv 里不该出现 KnownHostsFile：{av:?}"
        );
        assert!(
            av.iter().any(|a| a == "StrictHostKeyChecking=accept-new"),
            "StrictHostKeyChecking=accept-new 必须保留：{av:?}"
        );
    }

    /// **终端走哪条路**（2026-11-11 用户拍板：默认并入共享连接，
    /// 留一个开关切回 spawn）。
    #[test]
    fn 终端走哪条路() {
        use super::TermSsh;
        // 1) 用户明确选了 spawn ⇒ 永远 spawn（哪怕后端支持并入）
        assert_eq!(
            super::decide_term_ssh(Some("spawn"), true),
            TermSsh::Spawn,
            "用户选了 spawn ⇒ 哪怕后端支持也应当 spawn"
        );
        // 2) 用户选了 shared + 后端支持 ⇒ 并入共享连接
        assert_eq!(
            super::decide_term_ssh(Some("shared"), true),
            TermSsh::Shared
        );
        // 3) 用户选了 shared + 后端不支持 ⇒ 退回 spawn
        //    （宁可退回，也别开不出终端）
        assert_eq!(
            super::decide_term_ssh(Some("shared"), false),
            TermSsh::Spawn,
            "后端不支持进程内 SSH ⇒ 应当退回 spawn"
        );
        // 4) 没设 + 后端支持 ⇒ 默认并入共享连接（用户拍板的默认）
        assert_eq!(
            super::decide_term_ssh(None, true),
            TermSsh::Shared,
            "默认应当并入共享连接（一个服务器一条连接）"
        );
        // 5) 没设 + 后端不支持（Linux 的 VTE，阶段 B2 之前）⇒ spawn（现状不变）
        assert_eq!(
            super::decide_term_ssh(None, false),
            TermSsh::Spawn,
            "后端不支持 ⇒ 维持现状（spawn）"
        );
    }

    /// **重连后要不要发「回到原目录」的 `cd`**（纯逻辑）。
    ///
    /// ⚠️ 远程命令 / `-N` 的会话里**没有 shell** ⇒ 那行 `cd` 会被打进
    /// 正在运行的程序（比如 vim）⇒ 绝不能发。
    #[test]
    fn 恢复目录只在交互式shell里发() {
        assert!(super::restore_allowed(None, false), "普通 ssh 会话应当能发");
        assert!(
            super::restore_allowed(Some(""), false),
            "空的远程命令等同于没有远程命令"
        );
        assert!(
            !super::restore_allowed(Some("ls -l"), false),
            "远程命令会话里不能发 cd"
        );
        assert!(!super::restore_allowed(None, true), "-N（不要 shell）不能发 cd");
    }

    /// **用户的显式选择优先**（「在此处打开终端」挑了目录就别再覆盖它）。
    #[test]
    fn 用户挑了目录就不发恢复目录() {
        assert_eq!(
            super::pick_restore_dir(true, false, Some("/var/log")),
            Some("/var/log".to_string())
        );
        assert_eq!(
            super::pick_restore_dir(true, true, Some("/var/log")),
            None,
            "用户自己挑了目录 ⇒ 不该再发恢复目录的 cd"
        );
        assert_eq!(
            super::pick_restore_dir(false, false, Some("/var/log")),
            None,
            "非交互式 shell ⇒ 不发"
        );
        assert_eq!(super::pick_restore_dir(true, false, None), None);
        assert_eq!(super::pick_restore_dir(true, false, Some("")), None);
    }

    /// **askpass 助手**（阶段 C8）：ssh 在 tty 上问密码时，我们让它问**我们**
    /// ——于是**两个平台都是「我们的对话框问一次」**。
    ///
    /// 助手是**另一个进程** ⇒ 拿不到父进程的内存缓存 ⇒ 靠**会话级密码
    /// 文件**通气（父进程写、助手等）。
    #[test]
    fn askpass_助手能等会话文件() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("pp_askpass_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pw.tmp");

        // 环境变量里有密码 ⇒ 直接用，不等
        assert_eq!(
            super::askpass_answer(Some("env-pw".into()), Some(&path), Duration::from_millis(50)),
            "env-pw"
        );

        // 环境变量里没有 ⇒ 等文件
        let p2 = path.clone();
        let th = std::thread::spawn(move || {
            super::askpass_answer(None, Some(&p2), Duration::from_secs(5))
        });
        std::thread::sleep(Duration::from_millis(80));
        assert!(crate::pwcache::write_session_file_at(&path, "file-pw"));
        assert_eq!(th.join().unwrap(), "file-pw");

        // 等不到 ⇒ 交白卷（不能干等）
        let t0 = std::time::Instant::now();
        assert_eq!(
            super::askpass_answer(None, Some(&dir.join("never.tmp")), Duration::from_millis(200)),
            ""
        );
        assert!(t0.elapsed() < Duration::from_secs(5));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **spawn 出去的 ssh** 也得接上共享的密码通道（阶段 C8）。
    ///
    /// ⚠️ 以前只在"已经有密码"时才设 askpass ⇒ 密码未知时 ssh 在 tty 上
    /// 自己问 ⇒ 用户在终端里敲一次、SFTP 对话框再敲一次 ⇒ **两个地方、
    /// 两套体验**（Linux 与 Windows 还不一样）。
    #[test]
    fn spawn_路径的环境变量接上共享密码通道() {
        let mut o = crate::args::ConnOptions::default();
        o.proto = "ssh".into();
        o.host = "10.0.0.9".into();
        o.port = 22;
        o.user = "ops".into();

        // ① 已知密码 ⇒ 助手直接答
        let env = super::build_env_with_pw(&o, Some("known-pw"));
        let get = |k: &str| {
            env.iter()
                .find(|e| e.starts_with(&format!("{k}=")))
                .map(|e| e.splitn(2, '=').nth(1).unwrap_or("").to_string())
        };
        assert_eq!(get("SSH_ASKPASS_REQUIRE").as_deref(), Some("force"));
        assert_eq!(get("PUTTYPLUS_PW").as_deref(), Some("known-pw"));
        assert!(get("PUTTYPLUS_PW_FILE").is_none(), "有密码就不用等文件");

        // ② 密码未知 ⇒ 助手去等**会话级密码文件**
        let env = super::build_env_with_pw(&o, None);
        let get = |k: &str| {
            env.iter()
                .find(|e| e.starts_with(&format!("{k}=")))
                .map(|e| e.splitn(2, '=').nth(1).unwrap_or("").to_string())
        };
        assert_eq!(get("SSH_ASKPASS_REQUIRE").as_deref(), Some("force"));
        assert!(get("PUTTYPLUS_PW").is_none());
        let f = get("PUTTYPLUS_PW_FILE").expect("必须给出会话文件路径");
        assert!(f.contains("pp-pw-"), "会话文件路径不对：{f}");

        // ③ 有私钥 ⇒ 不该问密码（别把 askpass 塞进去）
        o.keyfile = Some("/home/x/.ssh/id_ed25519".into());
        let env = super::build_env_with_pw(&o, None);
        assert!(
            !env.iter().any(|e| e.starts_with("SSH_ASKPASS=")),
            "有私钥时不该启用 askpass"
        );
    }

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
