//! SSH 连接失败的**原因分类** ＋ 给用户看的一句话。
//!
//! ## 为什么要有
//!
//! 用户看到「SSH 连接失败：连接 10.0.0.9:22 失败: 超时」时，**不知道该干什么**
//! ——重试？换网络？改密码？⇒ 按原因给一句人话，再给一颗「重试」按钮，
//! 错误恢复就闭环了（`panel::dialogs::show_error_with_retry`）。
//!
//! ## 两条路的错误来源不一样
//!
//! - **进程内 SSH**（russh）：错误是**我们自己生成的**（中文/英文随界面语言）。
//! - **外部 ssh/plink**：错误是 **ssh 自己**打进终端的（英文原文，如
//!   `ssh: connect to host x port 22: Connection timed out`）。
//!
//! ⇒ 两边都要认得出来 ⇒ 关键词表**中英文都收**（别只认中文：英文界面下
//! 错误文本也是英文的）。

use crate::i18n::tr;

/// SSH 连接失败的原因。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// **超时**：服务器没在预期时间内回应（登录超时、网络黑洞）。
    Timeout,
    /// **认证没过**：密码/私钥不对，或服务器不认这个账号。
    Auth,
    /// **网络不通**：拒绝连接、不可达、域名解析不了。
    Network,
    /// 其它（说不清原因）。
    Unknown,
}

/// 超时的说法（中英文都收）。
const TIMEOUT_WORDS: &[&str] = &[
    "超时",
    "timed out",
    "timeout",
    "no response",
    "未响应",
];

/// 认证没过的说法。
///
/// ⚠ **中文认「认证失败」而不是光认「认证」**：配置错误
///    （「认证方式是『只用指定的私钥文件』，但这条连接没选私钥」）
///    里也有「认证」两个字 ⇒ 认得太宽会把**配置问题**说成**密码错了**。
const AUTH_WORDS: &[&str] = &[
    "认证失败",
    "认证没通过",
    "认证错误",
    "authentication",
    "permission denied",
    "publickey",
    "host key verification failed",
    "too many authentication failures",
];

/// 网络不通的说法。
const NETWORK_WORDS: &[&str] = &[
    "拒绝连接",
    "connection refused",
    "no route to host",
    "network is unreachable",
    "could not resolve",
    "name or service not known",
    "解析",
    "unreachable",
];

/// 从一段错误文本里认出原因。
///
/// ⚠ **顺序有讲究**：认证超时（`认证失败: …（超时）`）里**同时**有
/// 「认证」和「超时」⇒ 超时更贴近用户看到的现象（他等了很久）⇒ 超时排前面。
pub fn classify(text: &str) -> Kind {
    let t = text.to_ascii_lowercase();
    if TIMEOUT_WORDS.iter().any(|w| t.contains(w)) {
        return Kind::Timeout;
    }
    if AUTH_WORDS.iter().any(|w| t.contains(w)) {
        return Kind::Auth;
    }
    if NETWORK_WORDS.iter().any(|w| t.contains(w)) {
        return Kind::Network;
    }
    Kind::Unknown
}

/// 给用户看的一句话（**原因 → 下一步**）。
pub fn hint(kind: Kind) -> &'static str {
    match kind {
        Kind::Timeout => tr("服务器一直没回应：网络可能不通、服务器没起来，或被防火墙挡了。"),
        Kind::Auth => tr("认证没通过：密码或私钥不对，或服务器不允许这个账号登录。"),
        Kind::Network => tr("连不上服务器：地址不通、端口被拒绝，或域名解析不了。"),
        Kind::Unknown => tr("SSH 连接失败。"),
    }
}

/// 弹框正文：**一句人话** ＋ **原始错误**（要能复制走查问题）。
///
/// ⚠ **分隔符在代码里拼**，别做成 `tr()` 的模板 ⇒ 英文界面下模板查不到
/// 译文就会整段显示中文。
pub fn message(kind: Kind, raw: &str) -> String {
    let mut s = hint(kind).to_string();
    let raw = raw.trim();
    if !raw.is_empty() {
        s.push('\n');
        s.push('\n');
        s.push_str(raw);
    }
    s
}

/// ssh 连不上时用的退出码（**OpenSSH 的约定**：任何连接错误都退 255）。
pub const SSH_ERROR_EXIT: i32 = 255;

/// 这个退出状态是不是「ssh 自己报的错」。
///
/// ⚠ **两种形状都要认**：
/// - VTE 的 `child-exited` 信号给的是 **waitpid 的原始状态**
///   （正常退出码 N ⇒ `N << 8`；被信号杀掉 ⇒ 信号号本身）；
/// - 别的后端（ghostty 那条路）给的可能就是**退出码本身**。
///
/// ⇒ 两种都判一下，别把「连不上」当成「正常结束」放过去。
pub fn is_ssh_error_exit(raw: i32) -> bool {
    raw == SSH_ERROR_EXIT || raw == (SSH_ERROR_EXIT << 8)
}

/// 把「子进程退出状态」折算成**给人看的退出码**。
///
/// ⚠ 见 [`is_ssh_error_exit`] 的说明：VTE 给的可能是 waitpid 的原始状态
/// （退出码 N ⇒ `N << 8`）⇒ 折算一下再显示，别把 65280 摆到用户眼前。
pub fn exit_code(raw: i32) -> i32 {
    if raw >= 0x100 {
        raw >> 8
    } else {
        raw
    }
}

/// 从屏幕文字的**尾部**挑出 ssh 自己打的报错（外部 ssh 那条路）。
///
/// ⚠ 屏幕上可能还有用户自己的输出 ⇒ 只取**尾部**几行（ssh 的报错总在最后）。
/// 返回空串 = 屏幕上没看出 ssh 的报错。
pub fn ssh_error_tail(screen: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim_end)
        .filter(|s| !s.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// ssh/plink 自己会打的那些行（**行首特征**）。
///
/// ⚠ **只认行首特征**，不认泛泛的关键词 ⇒ 用户自己的输出里出现
///    「Connection refused」不该被当成「SSH 连不上」。
const SSH_ERROR_PREFIXES: &[&str] = &[
    "ssh:",
    "plink:",
    "fatal error:",
    "unable to connect to",
    "no more authentication methods to try",
];

/// ssh/plink 自己会打的那几句话（**行内特征**，没法靠行首认的）。
const SSH_ERROR_PHRASES: &[&str] = &[
    "host key verification failed",
    "permission denied (publickey",
    "permission denied (password",
    "permission denied (keyboard-interactive",
    "too many authentication failures",
];

/// 这一行是不是 **ssh/plink 自己报的错**。
pub fn is_ssh_error_line(line: &str) -> bool {
    let l = line.trim().to_ascii_lowercase();
    if l.is_empty() {
        return false;
    }
    SSH_ERROR_PREFIXES.iter().any(|p| l.starts_with(p))
        || SSH_ERROR_PHRASES.iter().any(|p| l.contains(p))
}

/// 从屏幕文字里找出 **ssh 自己报错的那一行**（外部 ssh 那条路）。
///
/// ⚠ **为什么不能靠退出码判断**（实测，阶段 154）：VTE 的 `child-exited`
///    给的是 waitpid 的原始状态（退出码 N ⇒ `N<<8`），可**子进程秒退**时
///    那个信号要么不来、要么给 0 ⇒ 「ssh 连不上」这种**最快的失败**
///    恰恰是退出码最靠不住的时候 ⇒ 只能看 **ssh 自己打的报错**。
///
/// 返回 None = 屏幕上没看出 ssh 的报错（那是正常的会话结束，别乱弹框）。
///
/// ⚠️⚠️ **空行必须先滤掉**（2026-11-26 修的真 bug，**CI 上必红**）：
///   终端屏幕**底部常有一堆空行**（VTE 可见区没内容就是空的）⇒ 若按
///   「屏幕的最后 N 行」取 ⇒ 那 N 行**全是空行** ⇒ 真正那句
///   `ssh: connect to host …`（它在屏幕**上面**）**被顶出去** ⇒
///   认不出原因 ⇒ 用户只看到「ssh 客户端退出码 255」（**没有原因**）。
///   ⇒ 实测：同一段内容，**屏幕底部有没有空行 ⇒ 结论完全相反**
///   （本机普通 HOME 底部没空行 ⇒ 碰巧过；CI 的 HOME 底部有空行 ⇒ 红）。
///   ⇒ ⇒ **先滤掉空行，再取尾部 N 行**（保住「只看尾部」的语义，
///   又不被空行顶出去）。
pub fn find_ssh_error_line(screen: &str, max_lines: usize) -> Option<(Kind, String)> {
    let lines: Vec<&str> = screen
        .lines()
        .map(str::trim_end)
        .filter(|s| !s.trim().is_empty())
        .collect();
    let start = lines.len().saturating_sub(max_lines);
    for line in &lines[start..] {
        if is_ssh_error_line(line) {
            let text = line.trim().to_string();
            return Some((classify(&text), text));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 认得出超时() {
        assert_eq!(classify("连接 10.0.0.9:22 失败: 超时"), Kind::Timeout);
        assert_eq!(
            classify("ssh: connect to host 10.0.0.9 port 22: Connection timed out"),
            Kind::Timeout
        );
        assert_eq!(
            classify("认证失败: 服务器未响应认证请求（超时）"),
            Kind::Timeout,
            "认证超时更贴近「他等了很久」⇒ 归超时"
        );
    }

    #[test]
    fn 认得出认证失败() {
        assert_eq!(classify("认证失败（密码认证未通过）"), Kind::Auth);
        assert_eq!(
            classify("Permission denied (publickey,password)."),
            Kind::Auth
        );
        assert_eq!(classify("Host key verification failed."), Kind::Auth);
    }

    #[test]
    fn 认得出网络不通() {
        assert_eq!(
            classify("连接 10.0.0.9:22 失败: Connection refused"),
            Kind::Network
        );
        assert_eq!(
            classify("ssh: Could not resolve hostname foo: Name or service not known"),
            Kind::Network
        );
    }

    #[test]
    fn 认不出就老实说不知道() {
        assert_eq!(
            classify("认证方式是「只用指定的私钥文件」，但这条连接没选私钥"),
            Kind::Unknown,
            "配置错误不是「认证失败」⇒ 别把配置问题说成密码错了"
        );
        assert_eq!(classify(""), Kind::Unknown);
    }

    #[test]
    fn 退出码两种形状都认() {
        // VTE 给的原始状态（255 << 8）
        assert!(is_ssh_error_exit(255 << 8));
        // 别的后端可能直接给退出码
        assert!(is_ssh_error_exit(255));
        // 正常结束不算
        assert!(!is_ssh_error_exit(0));
        assert!(!is_ssh_error_exit(1));
    }

    #[test]
    fn 退出码折算() {
        assert_eq!(exit_code(0), 0);
        assert_eq!(exit_code(255), 255);
        assert_eq!(exit_code(255 << 8), 255);
        assert_eq!(exit_code(7 << 8), 7);
    }

    #[test]
    fn 屏幕尾部才是ssh的报错() {
        let screen = "user@host:~$ ls\nREADME\nssh: connect to host x port 22: Connection timed out\n";
        // 只要最后一行 ⇒ ssh 的报错那一行
        let tail = ssh_error_tail(screen, 1);
        assert!(tail.contains("Connection timed out"), "尾部要含 ssh 的报错：{tail}");
        assert!(!tail.contains("README"), "太靠前的行不该进来：{tail}");
        // 要两行时，挨着报错的那行也带上
        let tail2 = ssh_error_tail(screen, 2);
        assert!(tail2.contains("README"), "两行就带上前一行：{tail2}");
    }

    #[test]
    fn 认得出ssh自己打的那些行() {
        assert!(is_ssh_error_line("ssh: connect to host x port 22: Connection timed out"));
        assert!(is_ssh_error_line(
            "ssh: Could not resolve hostname foo: Name or service not known"
        ));
        assert!(is_ssh_error_line("user@host: Permission denied (publickey,password)."));
        assert!(is_ssh_error_line("Host key verification failed."));
        assert!(is_ssh_error_line("Unable to connect to 10.0.0.9:22: Connection refused"));
        assert!(is_ssh_error_line("FATAL ERROR: Network error: Connection refused"));
    }

    #[test]
    fn 用户自己的输出不算ssh报错() {
        assert!(!is_ssh_error_line("Connection refused"), "泛泛的话不算");
        assert!(!is_ssh_error_line("ls: cannot access 'x': No such file or directory"));
        assert!(!is_ssh_error_line(""));
        assert!(!is_ssh_error_line("   "));
    }

    #[test]
    fn 从屏幕里找出ssh报错行() {
        let screen = "user@host:~$ ls\nREADME\nssh: connect to host x port 22: Connection timed out\n";
        let (kind, line) = find_ssh_error_line(screen, 6).expect("应当找得到 ssh 的报错");
        assert_eq!(kind, Kind::Timeout);
        assert!(line.contains("Connection timed out"), "找错了行：{line}");

        // 屏幕里没有 ssh 报错 ⇒ 不该有 ⇒ 免得误弹框
        assert!(find_ssh_error_line("user@host:~$ ls\nREADME\n", 6).is_none());
    }

    /// ⚠️ **屏幕底部有一堆空行** ⇒ 报错那句在屏幕**上面** ⇒
    ///   **照样要认得出**（2026-11-26：CI 上就是栽在这里 ⇒ 必红）。
    #[test]
    fn 屏幕底部全是空行也认得出() {
        let mut screen = String::from("ssh: connect to host 127.0.0.1 port 1: Connection refused\n");
        screen.push('\n');
        screen.push_str("== 会话已结束（退出码 255）。点上方「重新连接」可重建会话 ==\n");
        // 屏幕底部还有 20 多行空行（VTE 可见区没内容就是空的）
        for _ in 0..22 {
            screen.push('\n');
        }
        let (kind, line) = find_ssh_error_line(&screen, 6).expect("底部全是空行也要认得出");
        assert_eq!(kind, Kind::Network);
        assert!(
            line.contains("Connection refused"),
            "认错了行：{line}"
        );

        // 尾部那几行也一样（空行不算内容）
        let tail = ssh_error_tail(&screen, 1);
        assert!(
            tail.contains("会话已结束"),
            "尾部要含最后那行内容：{tail}"
        );
        assert!(
            !tail.contains("Connection refused"),
            "只取一行就不该带上更上面的报错：{tail}"
        );
        // 取两行 ⇒ 两行都要（滤空行后，非空行本来就只有这两行）
        let tail2 = ssh_error_tail(&screen, 2);
        assert!(
            tail2.contains("会话已结束") && tail2.contains("Connection refused"),
            "取两行要把两行都带上：{tail2}"
        );
    }
}
