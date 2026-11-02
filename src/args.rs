//! 与 PuTTY 兼容的命令行参数解析（对应 C 版 args.c 的 argv 部分）。
//!
//! 覆盖堡垒机拉起 putty 时常见的全部参数；不认识的参数记录警告后忽略，
//! 保证程序不会因为堡垒机多传了某个 GUI 专用参数而退出。

use std::path::Path;

use zeroize::Zeroizing;

use crate::config::{
    load_putty_session_with_home, resolve_ssh_config_alias_with_home,
};
use crate::util::atoi_like;

#[derive(Debug, Clone)]
pub struct ConnOptions {
    /// "ssh" / "telnet" / "rlogin" / "raw" / "serial"
    pub proto: String,
    pub host: String,
    pub user: String,
    /// 0 表示未指定（默认 22）；与 C 版一致用 atoi 语义（可超范围/为负，原样透传）
    pub port: i32,
    /// 来自 -pw / -pwfile / 会话文件 / SSHPASS，可为 None；释放时抹零
    pub password: Option<Zeroizing<String>>,
    /// -i
    pub keyfile: Option<String>,
    /// -proxycmd
    pub proxycmd: Option<String>,
    /// -load 或位置参数解析出的会话名
    pub session_name: Option<String>,
    /// -title（窗口标题，堡垒机 launcher 会传 账号@设备IP）
    pub title: Option<String>,

    pub no_shell: bool,  /* -N */
    pub force_pty: bool, /* -t */
    pub no_pty: bool,    /* -T */
    pub ipv4: bool,
    pub ipv6: bool,
    pub compress: bool, /* -C */
    pub verbose: bool,  /* -v */
    pub batch: bool,    /* -batch */

    /// 元素为 "L:spec" / "R:spec" / "D:spec"
    pub forwards: Vec<String>,

    /// **进程内**端口映射规则（元素同上，内部形）。
    ///
    /// 与 `forwards` 的区别：`forwards` 交给终端 spawn 的 ssh/plink 承接
    /// （CLI 参数必须与 C 基线逐字节一致，不能塞新东西）；这里的规则
    /// 跑在**应用自己那条 SSH 连接**上——不新建 SSH 连接、终端标签页
    /// 关了也照样在，堡垒机场景下正是要这个。
    ///
    /// 只能来自连接配置（connections.ini 的 inprocess_forwardings 键），
    /// CLI 没法配（命令行参数是兼容基线，不能动）。
    pub inproc_forwards: Vec<String>,

    /// 串口参数（proto == "serial" 时生效）
    pub serial_baud: u32,
    pub serial_data: u8,
    pub serial_parity: char, // n/e/o/m/s
    pub serial_stop: u8,     // 1 / 2
    pub serial_flow: char,   // n(无)/x(xon/xoff)/r(rtscts)/d

    /// -m 文件内容
    pub remote_cmd: Option<String>,

    /// 保活间隔（秒）；0 = 关闭。此前写死 30（1.5 起可按会话覆盖）
    pub keepalive_sec: u32,
    /// 终端类型（TERM）。None = 内置默认 xterm-256color（2.2 可覆盖）。
    /// 只影响 spawn 出的 ssh/plink 与自桥接子进程，远端 pty 类型由它决定。
    pub term_type: Option<String>,
    /// 认证优先级：密码优先（默认，与 C 版一致）还是密钥优先（2.2）。
    /// 只影响进程内 russh 链路，spawn 出去的 ssh 不受此影响。
    pub auth_order: AuthOrder,
    /// 认证方式：允许用哪些凭据（阶段 70，审计 P1-5）。
    /// 只影响进程内 russh 链路（spawn 出去的 ssh 按 ~/.ssh/config 与命令行来）。
    pub auth_method: AuthMethod,
    /// 终端字号；None = 用内置默认（10.5）。会话级覆盖，不进 --print-config
    /// （C 版没有对应项，加进去会破坏兼容基线）
    pub font_size: Option<f64>,
    /// 终端响铃；None = 沿用 VTE 默认（开）
    pub bell: Option<bool>,
    /// 这条连接要不要记进「最近连接」。
    ///
    /// ⚠️ **默认 true**：界面上点开的连接当然要记。
    ///
    /// ⚠️ **命令行启动的连接默认不记**（堡垒机场景）：堡垒机的 launcher
    /// 每次拉起都是一条**一次性**的设备连接（`user@设备IP`），全都记下来
    /// 只会把「最近连接」冲垮、想找自己常用的那条反而找不到。
    /// ⇒ 由启动流程（`main.rs`）按设置决定，见
    /// [`crate::settings::record_cli_recent`]。
    ///
    /// **不在 `--print-config` 里**：命令行输出是 C 版兼容基线，
    /// 多一行就破坏契约（见 AGENTS.md）。
    pub record_recent: bool,
}

/// 「这是我们自己派生的窗口」的环境变量标记。
///
/// ⚠️ 「在新窗口打开」是**用户在界面里点的** ⇒ 那条连接该照常记进
/// 「最近连接」；但它是用命令行 spawn 起来的（密码走
/// `PUTTYPLUS_PW_SESSION`）⇒ 不带这个标记的话，会跟"堡垒机从外面拉起
/// 的窗口"混为一谈，一起被当成"命令行启动"而不记录。
pub const INTERNAL_SPAWN_ENV: &str = "PUTTYPLUS_INTERNAL_SPAWN";

impl ConnOptions {
    /// 会话级覆盖的键：会话名/标题优先，否则 host:port。
    /// 同一台机器用不同账号开的两个标签要能各自配置，所以带上 user 之外的
    /// 区分维度；同名会话则共用一份覆盖（与 connections.ini 的 name 对齐）。
    pub fn session_key(&self) -> String {
        for c in [self.session_name.as_deref(), self.title.as_deref()] {
            if let Some(c) = c.filter(|s| !s.trim().is_empty()) {
                return c.to_string();
            }
        }
        format!("{}:{}", self.host, self.port)
    }
}

impl Default for ConnOptions {
    fn default() -> Self {
        ConnOptions {
            proto: "ssh".into(),
            host: String::new(),
            user: String::new(),
            port: 0,
            password: None,
            keyfile: None,
            proxycmd: None,
            session_name: None,
            title: None,
            no_shell: false,
            force_pty: false,
            no_pty: false,
            ipv4: false,
            ipv6: false,
            compress: false,
            verbose: false,
            batch: false,
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            keepalive_sec: 30,
            term_type: None,
            auth_order: AuthOrder::default(),
            auth_method: AuthMethod::default(),
            font_size: None,
            bell: None,
            record_recent: true,
            serial_baud: 115200,
            serial_data: 8,
            serial_parity: 'n',
            serial_stop: 1,
            serial_flow: 'n',
            remote_cmd: None,
        }
    }
}

/// 认证优先级（2.2）。默认 PasswordFirst 与 C 版行为完全一致。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthOrder {
    /// 密码/键盘交互优先（默认）：既少一次无谓的公钥尝试，也避免把
    /// 本地身份出示给堡垒机网关一类中介
    #[default]
    PasswordFirst,
    /// 密钥优先：先试私钥与 agent，失败再回落到密码
    PublicKeyFirst,
}

impl AuthOrder {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthOrder::PasswordFirst => "password-first",
            AuthOrder::PublicKeyFirst => "publickey-first",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "password-first" | "password" | "pw" => Some(AuthOrder::PasswordFirst),
            "publickey-first" | "publickey" | "key" => Some(AuthOrder::PublicKeyFirst),
            _ => None,
        }
    }
}

/// 认证方式（阶段 70，审计 P1-5）：**允许用哪些凭据**。
///
/// ⚠️ 与 [`AuthOrder`] 是两个维度：认证**顺序**是"先试哪个"，
/// 认证**方式**是"哪些可以试"（比如"只准用密码"就别把本地身份
/// 出示给堡垒机网关）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMethod {
    /// 自动（默认）：按认证优先级依次尝试，与以前的为完全一致。
    #[default]
    Auto,
    /// 只用 ssh-agent 里的密钥（不试密码，也不试磁盘上的私钥）。
    Agent,
    /// 只用指定的那一个私钥文件（不试 agent，也不试密码）。
    KeyFile,
    /// 只用密码（不试任何公钥）。
    Password,
}

impl AuthMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuthMethod::Auto => "auto",
            AuthMethod::Agent => "agent",
            AuthMethod::KeyFile => "keyfile",
            AuthMethod::Password => "password",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" | "" => Some(AuthMethod::Auto),
            "agent" => Some(AuthMethod::Agent),
            "keyfile" | "key" => Some(AuthMethod::KeyFile),
            "password" | "pw" => Some(AuthMethod::Password),
            _ => None,
        }
    }

    /// 界面上的名字（下拉框里那几项）。
    pub fn label(&self) -> &'static str {
        match self {
            AuthMethod::Auto => "自动（按认证优先级依次尝试）",
            AuthMethod::Agent => "只用 ssh-agent 里的密钥",
            AuthMethod::KeyFile => "只用指定的私钥文件",
            AuthMethod::Password => "只用密码",
        }
    }
}

/// -sercfg 解析结果（串口参数）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SerialConf {
    pub baud: u32,
    pub data: u8,
    pub parity: char,
    pub stop: u8,
    pub flow: char,
}

/// -sercfg 值解析："波特率[,数据位[,校验[,停止位[,流控]]]]"，逐段可选；
/// 段缺失或非法时回落默认（非法段丢弃而不是报错，与 PuTTY 一致）。
pub fn parse_sercfg(val: &str) -> SerialConf {
    let mut it = val.split(',').map(str::trim);
    let baud = it
        .next()
        .map(atoi_like)
        .filter(|b| *b > 0)
        .map(|b| b as u32)
        .unwrap_or(115200);
    let data = it
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|d| (5..=8).contains(d))
        .unwrap_or(8);
    let parity = it
        .next()
        .and_then(|s| s.chars().next())
        .map(|c| c.to_ascii_lowercase())
        .filter(|c| "neoms".contains(*c))
        .unwrap_or('n');
    let stop = it
        .next()
        .and_then(|s| s.parse::<u8>().ok())
        .filter(|s| *s == 1 || *s == 2)
        .unwrap_or(1);
    let flow = match it.next().map(str::to_ascii_lowercase).as_deref() {
        Some("x" | "xonxoff") => 'x',
        Some("r" | "rtscts") => 'r',
        Some("d" | "dsrdtr") => 'd',
        Some("n" | "none") => 'n',
        _ => 'n',
    };
    SerialConf {
        baud,
        data,
        parity,
        stop,
        flow,
    }
}

impl ConnOptions {
    pub fn is_ssh(&self) -> bool {
        self.proto.eq_ignore_ascii_case("ssh")
    }

    /// 密码内容（Zeroizing 包装的便捷读取）。
    pub fn password_str(&self) -> Option<&str> {
        self.password.as_ref().map(|p| p.as_str())
    }

    /// host 是否为空（此时应弹出连接对话框）
    pub fn need_dialog(&self) -> bool {
        self.host.is_empty()
    }

    /// 解析 argv（不含 argv[0]）；warnings 收集人类可读警告。
    pub fn parse(args: &[String], warnings: &mut Vec<String>) -> ConnOptions {
        let sshpass = std::env::var("SSHPASS").ok().filter(|s| !s.is_empty());
        ConnOptions::parse_impl(args, &glib::home_dir(), sshpass.as_deref(), warnings)
    }

    /// 可注入 home 与 SSHPASS 的解析核心，便于单测。
    pub fn parse_impl(
        args: &[String],
        home: &Path,
        sshpass: Option<&str>,
        warnings: &mut Vec<String>,
    ) -> ConnOptions {
        let mut o = ConnOptions::default();
        let mut i = 0usize;
        let mut end_of_opts = false;
        let mut host_from_session = false; // host 由会话（位置参数形式）提供

        while i < args.len() {
            let a = args[i].as_str();

            if end_of_opts || !a.starts_with('-') {
                // 位置参数：第一个是 [user@]host 或会话名，其后视为远程命令。
                // 与真 PuTTY 语义一致：命令行 host 覆盖会话里的 HostName
                let mut pos = a;
                if o.host.is_empty() || host_from_session {
                    if let Some(at) = pos.rfind('@') {
                        if o.user.is_empty() {
                            o.user = pos[..at].to_string();
                        }
                        pos = &pos[at + 1..];
                    }
                    if !pos.is_empty() {
                        if host_from_session {
                            o.host = pos.to_string();
                            host_from_session = false;
                        } else if !load_putty_session_with_home(&mut o, pos, home, warnings) {
                            o.host = pos.to_string();
                        } else {
                            o.session_name = Some(pos.to_string());
                            if !o.host.is_empty() {
                                host_from_session = true;
                            }
                            warnings.push(format!("已加载 PuTTY 会话: {pos}"));
                        }
                    }
                } else if let Some(cmd) = o.remote_cmd.as_mut() {
                    *cmd = format!("{cmd} {pos}");
                } else {
                    o.remote_cmd = Some(pos.to_string());
                }
                i += 1;
                continue;
            }

            // -title=xxx 连值形式
            if let Some(t) = a.strip_prefix("-title=") {
                o.title = Some(t.to_string());
                i += 1;
                continue;
            }

            // 无值开关
            match a {
                // ⚠️ **-serial 是不带值的协议开关**（与 C 版 args.c 逐字一致）：
                // 串口设备路径走**位置参数**——`puttyplus -serial /dev/ttyUSB0`。
                // 以前 Rust 版让它**吃掉后一个参数**当设备路径 ⇒
                // `puttyplus -serial -P 115200 /dev/ttyUSB0` 会把 "-P" 当成设备
                // 路径（端口与设备全乱），`-serial user@/dev/ttyUSB0` 也不会像
                // C 版那样拆出 user。⇒ 协议开关一律不带值，设备走位置参数。
                "-ssh" | "-telnet" | "-rlogin" | "-raw" | "-serial" => {
                    o.proto = a[1..].to_string();
                    i += 1;
                    continue;
                }
                "-N" => {
                    o.no_shell = true;
                    i += 1;
                    continue;
                }
                "-t" => {
                    o.force_pty = true;
                    i += 1;
                    continue;
                }
                "-T" => {
                    o.no_pty = true;
                    i += 1;
                    continue;
                }
                "-4" => {
                    o.ipv4 = true;
                    i += 1;
                    continue;
                }
                "-6" => {
                    o.ipv6 = true;
                    i += 1;
                    continue;
                }
                "-C" => {
                    o.compress = true;
                    i += 1;
                    continue;
                }
                "-v" => {
                    o.verbose = true;
                    i += 1;
                    continue;
                }
                "-batch" => {
                    o.batch = true;
                    i += 1;
                    continue;
                }
                // 接受但忽略
                "-A" | "-a" | "-1" | "-2" | "-no-antispoof" | "-share" | "-noshare" | "-beep"
                | "-logappend" | "-noagent" | "-agent" => {
                    i += 1;
                    continue;
                }
                "--" => {
                    end_of_opts = true;
                    i += 1;
                    continue;
                }
                _ => {}
            }

            // 需要一个值的参数
            const VAL_FLAGS: [&str; 15] = [
                "-P", "-l", "-pw", "-pwfile", "-i", "-m", "-L", "-R", "-D", "-load",
                "-proxycmd", "-hostkey", "-sercfg", "-title", "-loghost",
            ];
            if !VAL_FLAGS.contains(&a) && a != "-sessionlog" {
                // 未知参数：警告并跳过（不吞值，保守处理）
                warnings.push(format!("忽略未知参数: {a}"));
                i += 1;
                continue;
            }
            let flag = a;
            let val = match args.get(i + 1) {
                Some(v) => {
                    i += 2;
                    Some(v.clone())
                }
                None => {
                    i += 1;
                    None
                }
            };
            let Some(val) = val else {
                warnings.push(format!("参数 {flag} 缺少值，已忽略"));
                continue;
            };

            match flag {
                "-P" => o.port = atoi_like(&val) as i32,
                "-l" => o.user = val,
                "-pw" => o.password = Some(Zeroizing::new(val)),
                "-pwfile" => match std::fs::read_to_string(&val) {
                    Ok(data) => {
                        // putty 语义：只取文件第一行，去掉行尾空白
                        let first = data.split('\n').next().unwrap_or("");
                        o.password = Some(Zeroizing::new(first.trim_end().to_string()));
                    }
                    Err(_) => warnings.push(format!("无法读取 -pwfile {val}")),
                },
                "-i" => {
                    o.keyfile = Some(crate::config::expand_tilde(&val, home));
                }
                "-m" => match std::fs::read_to_string(&val) {
                    Ok(data) => o.remote_cmd = Some(data),
                    Err(_) => warnings.push(format!("无法读取 -m 文件 {val}")),
                },
                "-L" => o.forwards.push(format!("L:{val}")),
                "-R" => o.forwards.push(format!("R:{val}")),
                "-D" => o.forwards.push(format!("D:{val}")),
                "-load" => {
                    o.session_name = Some(val.clone());
                    load_putty_session_with_home(&mut o, &val, home, warnings);
                }
                "-proxycmd" => o.proxycmd = Some(val),
                "-title" => o.title = Some(val),
                "-sercfg" => {
                    let c = parse_sercfg(&val);
                    o.serial_baud = c.baud;
                    o.serial_data = c.data;
                    o.serial_parity = c.parity;
                    o.serial_stop = c.stop;
                    o.serial_flow = c.flow;
                }
                // -loghost / -hostkey / -sessionlog：接受但忽略值
                _ => {}
            }
        }

        // putty 会话优先；非会话来源的主机名尝试 ~/.ssh/config 别名解析
        if o.session_name.is_none() && !o.host.is_empty() {
            resolve_ssh_config_alias_with_home(&mut o, home);
        }

        if o.port == 0 {
            o.port = match o.proto.as_str() {
                "telnet" => 23,
                "rlogin" => 513,
                _ => 22,
            };
        }
        if o.user.is_empty() {
            o.user = glib::user_name().to_string_lossy().into_owned();
        }
        // 环境变量密码兜底（sshpass -e 约定），仅当命令行与会话均未提供时
        if o.password_str().map_or(true, |p| p.is_empty()) {
            if let Some(ep) = sshpass.filter(|s| !s.is_empty()) {
                o.password = Some(Zeroizing::new(ep.to_string()));
                warnings.push("已从环境变量 SSHPASS 读取密码".into());
            }
        }
        o
    }

    /// 调试输出（--print-config），与 C 版逐行一致。
    pub fn print(&self) {
        println!(
            "proto={} host={} port={} user={}",
            self.proto, self.host, self.port, self.user
        );
        println!(
            "password={} keyfile={} session={}",
            if self.password.is_some() { "<set>" } else { "-" },
            self.keyfile.as_deref().unwrap_or("-"),
            self.session_name.as_deref().unwrap_or("-"),
        );
        println!("title={}", self.title.as_deref().unwrap_or("-"));
        println!(
            "flags: N={} t={} T={} v4={} v6={} C={} v={} batch={}",
            self.no_shell as i32,
            self.force_pty as i32,
            self.no_pty as i32,
            self.ipv4 as i32,
            self.ipv6 as i32,
            self.compress as i32,
            self.verbose as i32,
            self.batch as i32,
        );
        println!(
            "forwards={} remote_cmd={}",
            self.forwards.len(),
            if self.remote_cmd.is_some() { "<set>" } else { "-" },
        );
        for (i, f) in self.forwards.iter().enumerate() {
            println!("  fwd[{i}]={f}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testutil::temp_home;
    use std::fs;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    fn parse(list: &[&str]) -> (ConnOptions, Vec<String>) {
        let home = temp_home("args");
        let mut w = Vec::new();
        let o = ConnOptions::parse_impl(&args(list), &home, None, &mut w);
        (o, w)
    }

    /// 「这条连接要不要记进最近连接」默认**要记**——
    /// 界面上点开的连接当然要进「最近」；命令行启动的那条由
    /// **启动流程**（`main.rs`）按设置改掉。
    #[test]
    fn 默认要记最近连接() {
        let o = ConnOptions::default();
        assert!(o.record_recent, "默认要记");
        let (o2, _) = parse(&["ops@192.168.1.10"]);
        assert!(o2.record_recent, "解析出来的也默认要记");
    }

    #[test]
    fn user_at_host_positional() {        let (o, _) = parse(&["ops@192.168.1.10"]);
        assert_eq!(o.user, "ops");
        assert_eq!(o.host, "192.168.1.10");
        assert_eq!(o.port, 22);
        assert_eq!(o.proto, "ssh");
    }

    #[test]
    fn value_flags() {
        let (o, _) = parse(&["-P", "2222", "-l", "ops", "-pw", "secret", "-i", "/k",
                             "-title", "我的标题", "h"]);
        assert_eq!(o.port, 2222);
        assert_eq!(o.user, "ops");
        assert_eq!(o.password_str(), Some("secret"));
        assert_eq!(o.keyfile.as_deref(), Some("/k"));
        assert_eq!(o.title.as_deref(), Some("我的标题"));

        // 连值形式 -title=
        let (o2, _) = parse(&["-title=abc", "h"]);
        assert_eq!(o2.title.as_deref(), Some("abc"));
    }

    #[test]
    fn tilde_expand_keyfile() {
        let home = temp_home("tilde");
        let mut w = Vec::new();
        let o = ConnOptions::parse_impl(
            &args(&["-i", "~/.ssh/mykey", "h"]),
            &home,
            None,
            &mut w,
        );
        assert_eq!(
            o.keyfile.as_deref(),
            Some(home.join(".ssh/mykey").to_str().unwrap())
        );
    }

    #[test]
    fn remote_cmd_accumulates() {
        // 以 '-' 开头的词按未知参数处理，不会拼进远程命令（与 C 版一致）
        let (o, w) = parse(&["host", "echo", "hi"]);
        assert_eq!(o.host, "host");
        assert_eq!(o.remote_cmd.as_deref(), Some("echo hi"));
        assert!(w.is_empty());
    }

    #[test]
    fn proto_and_switches() {
        let (o, _) = parse(&["-telnet", "-N", "-t", "-4", "-C", "-v", "-batch", "h"]);
        assert_eq!(o.proto, "telnet");
        assert_eq!(o.port, 23);
        assert!(o.no_shell && o.force_pty && o.ipv4 && o.compress && o.verbose && o.batch);
        assert!(!o.is_ssh());
    }

    #[test]
    fn ignored_flags_and_unknown() {
        let (o, w) = parse(&["-A", "-a", "-1", "-2", "-noagent", "-agent", "-beep", "h"]);
        assert!(o.host == "h");
        assert!(w.is_empty(), "接受但忽略的参数不应告警");

        // 未知参数：警告且不吞掉后面的值
        let (o2, w2) = parse(&["-nosuch", "h"]);
        assert_eq!(o2.host, "h", "未知参数后的位置参数不应被吞");
        assert_eq!(w2.len(), 1);
        assert!(w2[0].contains("忽略未知参数"));
    }

    #[test]
    fn missing_value_warns() {
        let (_, w) = parse(&["-P"]);
        assert!(w.iter().any(|s| s.contains("缺少值")));
    }

    #[test]
    fn double_dash_ends_options() {
        let (o, _) = parse(&["-P", "2222", "--", "-weird-host"]);
        assert_eq!(o.port, 2222);
        assert_eq!(o.host, "-weird-host");
    }

    /// 2.2 认证优先级：默认必须仍是密码优先（与 C 版一致），
    /// parse/as_str 互为逆运算（未知值回落到默认而不是崩）
    #[test]
    fn auth_order_parse_roundtrip() {
        for o in [AuthOrder::PasswordFirst, AuthOrder::PublicKeyFirst] {
            assert_eq!(AuthOrder::parse(o.as_str()), Some(o), "{o:?}");
        }
        assert_eq!(AuthOrder::default(), AuthOrder::PasswordFirst);
        assert_eq!(AuthOrder::parse(""), None);
        assert_eq!(AuthOrder::parse("乱写"), None);
        assert_eq!(AuthOrder::parse(" PublicKey-First "), Some(AuthOrder::PublicKeyFirst));
    }

    /// 认证方式（阶段 70）：ini 里存的就是这几个词，读回来要一样。
    #[test]
    fn auth_method_roundtrip() {
        for m in [
            AuthMethod::Auto,
            AuthMethod::Agent,
            AuthMethod::KeyFile,
            AuthMethod::Password,
        ] {
            assert_eq!(AuthMethod::parse(m.as_str()), Some(m), "{m:?}");
            assert_eq!(AuthMethod::parse(&m.as_str().to_uppercase()), Some(m));
        }
        assert_eq!(AuthMethod::default(), AuthMethod::Auto);
        // 空 = 自动（缺键时的默认）
        assert_eq!(AuthMethod::parse(""), Some(AuthMethod::Auto));
        assert_eq!(AuthMethod::parse("乱写"), None);
        // 默认值是「自动」⇒ ConnOptions 的默认也就是自动
        assert_eq!(ConnOptions::default().auth_method, AuthMethod::Auto);
    }

    /// 1.5 把保活从写死改成可覆盖，但默认值必须还是 30——
    /// 变的是"能不能改"，不是"默认是什么"。
    #[test]
    fn session_knobs_defaults_preserve_old_behaviour() {
        let o = ConnOptions::default();
        assert_eq!(o.keepalive_sec, 30, "默认保活仍是 30s");
        assert_eq!(o.font_size, None);
        assert_eq!(o.bell, None);
        // 会话键：有名字用名字，没名字退回 host:port
        let mut o2 = o.clone();
        o2.host = "10.0.0.9".into();
        o2.port = 2222;
        assert_eq!(o2.session_key(), "10.0.0.9:2222");
        o2.title = Some("堡垒机".into());
        assert_eq!(o2.session_key(), "堡垒机");
        o2.session_name = Some("ops-box".into());
        assert_eq!(o2.session_key(), "ops-box", "session_name 优先于 title");
    }

    #[test]
    fn forwards() {
        let (o, _) = parse(&["-L", "9000:127.0.0.1:9000", "-R", "2222:h:22", "-D", "1080", "h"]);
        assert_eq!(o.forwards, vec!["L:9000:127.0.0.1:9000", "R:2222:h:22", "D:1080"]);
    }

    #[test]
    fn load_session_flag() {
        let home = temp_home("loadflag");
        fs::create_dir_all(home.join(".putty/sessions")).unwrap();
        fs::write(
            home.join(".putty/sessions/Bastion"),
            "HostName=10.0.0.9\nPortNumber=2222\nUserName=ops\n",
        )
        .unwrap();
        let mut w = Vec::new();
        let o = ConnOptions::parse_impl(&args(&["-load", "Bastion"]), &home, None, &mut w);
        assert_eq!(o.host, "10.0.0.9");
        assert_eq!(o.port, 2222);
        assert_eq!(o.user, "ops");
        assert_eq!(o.session_name.as_deref(), Some("Bastion"));
    }

    #[test]
    fn positional_session_then_host_override() {
        let home = temp_home("possession");
        fs::create_dir_all(home.join(".putty/sessions")).unwrap();
        fs::write(
            home.join(".putty/sessions/Bastion"),
            "HostName=10.0.0.9\nPortNumber=2222\n",
        )
        .unwrap();
        // 第一个位置参数是会话名，第二个覆盖会话里的 HostName
        let mut w = Vec::new();
        let o = ConnOptions::parse_impl(&args(&["Bastion", "1.2.3.4"]), &home, None, &mut w);
        assert_eq!(o.host, "1.2.3.4");
        assert_eq!(o.port, 2222, "会话提供的端口保留");
        assert_eq!(o.session_name.as_deref(), Some("Bastion"));
    }

    #[test]
    fn sshpass_fallback_only_when_unset() {
        let home = temp_home("sshpass");
        let mut w = Vec::new();
        let o = ConnOptions::parse_impl(&args(&["h"]), &home, Some("envpw"), &mut w);
        assert_eq!(o.password_str(), Some("envpw"));
        assert!(w.iter().any(|s| s.contains("SSHPASS")));

        let mut w2 = Vec::new();
        let o2 = ConnOptions::parse_impl(&args(&["-pw", "cli", "h"]), &home, Some("envpw"), &mut w2);
        assert_eq!(o2.password_str(), Some("cli"), "命令行密码优先");
        assert!(!w2.iter().any(|s| s.contains("SSHPASS")));
    }

    #[test]
    fn serial_args() {
        let (o, w) = parse(&["-serial", "/dev/ttyUSB0", "-sercfg", "9600,7,e,2"]);
        assert_eq!(o.proto, "serial");
        assert_eq!(o.host, "/dev/ttyUSB0");
        assert_eq!(o.serial_baud, 9600);
        assert_eq!(o.serial_data, 7);
        assert_eq!(o.serial_parity, 'e');
        assert_eq!(o.serial_stop, 2);
        assert_eq!(o.serial_flow, 'n');
        assert!(w.is_empty(), "已实现的参数不应告警: {w:?}");

        // 默认波特率 + 流控
        let (o2, _) = parse(&["-serial", "/dev/ttyS0", "-sercfg", "115200,8,n,1,xonxoff"]);
        assert_eq!(o2.serial_baud, 115200);
        assert_eq!(o2.serial_flow, 'x');

        // 仅波特率
        let (o3, _) = parse(&["-serial", "/dev/ttyS0"]);
        assert_eq!(o3.serial_baud, 115200);
        assert_eq!(o3.serial_data, 8);
    }

    /// **-serial 是不带值的协议开关**（C 版 args.c 的语义，阶段 95）。
    ///
    /// ⚠️ 以前 Rust 版让 `-serial` **吃掉后一个参数**当设备路径 ⇒
    /// `-serial -P 115200 /dev/ttyUSB0` 会把 "-P" 当成设备路径（端口与设备
    /// 全乱），`-serial user@/dev/ttyUSB0` 也不会像 C 版那样拆出 user。
    /// 设备路径必须走**位置参数**。
    #[test]
    fn serial_switch_takes_no_value() {
        // -serial 后面跟别的参数时，那些参数各归各位
        let (o, _) = parse(&["-serial", "-P", "115200", "/dev/ttyUSB0"]);
        assert_eq!(o.proto, "serial");
        assert_eq!(o.port, 115200, "端口不能被 -serial 吃掉");
        assert_eq!(o.host, "/dev/ttyUSB0", "设备路径走位置参数");
        assert!(o.remote_cmd.is_none(), "设备路径不该被当成远程命令");

        // 位置参数里带 user@ 时按 C 版拆开
        let (o2, _) = parse(&["-serial", "ops@/dev/ttyUSB0"]);
        assert_eq!(o2.user, "ops");
        assert_eq!(o2.host, "/dev/ttyUSB0");
    }

    #[test]
    fn parse_sercfg_cases() {
        let c = parse_sercfg("115200");
        assert_eq!(c, SerialConf { baud: 115200, data: 8, parity: 'n', stop: 1, flow: 'n' });
        let c = parse_sercfg("9600,7, EVEN ,2,r");
        assert_eq!(c.data, 7);
        assert_eq!(c.parity, 'e');
        assert_eq!(c.stop, 2);
        assert_eq!(c.flow, 'r');
        // 非法值回退默认
        let c = parse_sercfg("abc,99,x,9,q");
        assert_eq!(c.baud, 115200);
        assert_eq!(c.data, 8);
        assert_eq!(c.parity, 'n');
        assert_eq!(c.stop, 1);
        assert_eq!(c.flow, 'n');
    }

    #[test]
    fn ssh_config_alias_via_parse() {
        let home = temp_home("parsealias");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::write(home.join(".ssh/config"), "Host jb\n  HostName 10.1.1.1\n  Port 2200\n").unwrap();
        let mut w = Vec::new();
        let o = ConnOptions::parse_impl(&args(&["jb"]), &home, None, &mut w);
        assert_eq!(o.host, "10.1.1.1");
        assert_eq!(o.port, 2200);
    }
}
