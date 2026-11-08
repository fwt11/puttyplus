//! `~/.ssh/config` 别名解析与 PuTTY 会话文件加载（对应 C 版 args.c 的配置部分）。
//!
//! 终端侧的 ssh 会自行读取 `~/.ssh/config`；为保证 SFTP 侧连到同一目标、
//! 用同一端口/用户/密钥，这里按 OpenSSH 语义（首个匹配值生效）解析一次。

use std::path::{Path, PathBuf};

use crate::args::ConnOptions;
use crate::util::atoi_like;
use crate::trf;

/// 把开头的 `~` / `~/` 展开为用户主目录。
///
/// ⚠️ **只展开开头**：`~user/x` 原样保留（PuTTY/C 版契约）。
/// ⚠️ `home` 传进来是为了**可测**（测试里用临时目录，不碰用户真实 home）。
///
/// ⚠️ **分隔符用平台的**（Windows 是 `\`、Unix 是 `/`）：home 与其余部分
///    之间用平台分隔符，其余部分**原样保留**
///    （用户写 `~/.ssh/id_rsa`，里面的 `/` 就还是 `/`）。
///
///    ⚠️ 之前这里是纯 POSIX 拼接（`home + "/" + rest`）⇒ Windows 上得到
///    `C:\Users\bob/.ssh/id_rsa`（两种分隔符混用），跟同一份代码里
///    `Path::join` 的结果对不上 ⇒ Windows CI 的单测直接炸了。
pub fn expand_tilde(p: &str, home: &Path) -> String {
    expand_tilde_for(p, home, cfg!(windows))
}

/// 同 [`expand_tilde`]，但**显式指定是不是 Windows**。
///
/// ⚠️ 为什么要能指定：Windows 上的行为（分隔符是 `\`）必须**在 Linux 上
///    也能测到**，否则改错了要等一轮 Windows CI 才知道。
///    ⇒ 平台相关的部分用参数带进来，平台判断只留 [`expand_tilde`] 一处。
fn expand_tilde_for(p: &str, home: &Path, windows: bool) -> String {
    let sep = if windows { '\\' } else { '/' };
    if let Some(rest) = p.strip_prefix('~') {
        if rest.is_empty() || rest.starts_with('/') {
            let rest = rest.trim_start_matches('/');
            if rest.is_empty() {
                return home.to_string_lossy().into_owned();
            }
            let dir = home.to_string_lossy();
            let dir = dir.trim_end_matches(['/', '\\']);
            if dir.is_empty() {
                return rest.to_string();
            }
            return format!("{dir}{sep}{rest}");
        }
    }
    p.to_string()
}

/// `~/.ssh/config` 里的 `IdentityFile` 怎么变成一个路径。
///
/// - **绝对路径**（`/keys/id_rsa`、`C:\keys\id_rsa`）⇒ 原样用；
/// - **相对路径**（`rel_key`）⇒ 按 OpenSSH 语义补到 `~/.ssh/` 下。
///
/// ⚠️ **绝对路径的判定必须分平台**：Windows 上 home 展开出来的是
///    `C:\Users\bob\.ssh/id_rsa`（**盘符**开头，不是 `/` 开头）⇒
///    只判 `starts_with('/')` 会把它当成相对路径再补一层 `~/.ssh/`。
fn identity_path(kf: &str, home: &Path) -> String {
    let absolute = crate::platform::localpath::is_abs_shape(kf);
    if absolute {
        kf.to_string()
    } else {
        home.join(&format!(".ssh/{kf}"))
            .to_string_lossy()
            .into_owned()
    }
}

/// fnmatch(3) 的常用子集：`*`、`?`、`[seq]` / `[!seq]`（大小写敏感）。
/// OpenSSH 的 host 匹配实际只支持 `*` 和 `?`，这里多支持字符类以对齐 C 版行为。
pub fn fnmatch(pat: &str, text: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => m(&p[1..], t) || (!t.is_empty() && m(p, &t[1..])),
            Some(b'?') => !t.is_empty() && m(&p[1..], &t[1..]),
            Some(b'[') => {
                // 找闭括号；开头允许 ']' 作为字面成员，未闭合则按字面 '[' 处理
                let mut i = 1;
                let negate = matches!(p.get(i), Some(b'!') | Some(b'^'));
                if negate {
                    i += 1;
                }
                let start = i;
                if p.get(i) == Some(&b']') {
                    i += 1;
                }
                while i < p.len() && p[i] != b']' {
                    i += 1;
                }
                if i == p.len() {
                    // 未闭合：'[' 是字面字符
                    return t.first() == Some(&b'[') && m(&p[1..], &t[1..]);
                }
                let Some(&c) = t.first() else {
                    return false;
                };
                let mut hit = false;
                let mut j = start;
                while j < i {
                    if j + 2 < i && p[j + 1] == b'-' {
                        if c >= p[j] && c <= p[j + 2] {
                            hit = true;
                        }
                        j += 3;
                    } else {
                        if c == p[j] {
                            hit = true;
                        }
                        j += 1;
                    }
                }
                (hit != negate) && m(&p[i + 1..], &t[1..])
            }
            Some(&c) => t.first() == Some(&c) && m(&p[1..], &t[1..]),
        }
    }
    m(pat.as_bytes(), text.as_bytes())
}

/// OpenSSH 语义：任一负向 `!pattern` 命中立即否决；否则至少一个正向命中才算匹配。
fn host_patterns_match(pats: &[&str], target: &str) -> bool {
    let mut positive = false;
    for p in pats {
        if let Some(neg) = p.strip_prefix('!') {
            if fnmatch(neg, target) {
                return false;
            }
        } else if fnmatch(p, target) || *p == "*" {
            positive = true;
        }
    }
    positive
}

/// `~/.ssh/config` 别名展开：把 `o.host` 按配置改写为 HostName 并补 Port/User/
/// IdentityFile（仅当字段为空时）。host 不是别名时原样保留。
pub fn resolve_ssh_config_alias_with_home(o: &mut ConnOptions, home: &Path) {
    let path: PathBuf = [home, Path::new(".ssh"), Path::new("config")]
        .iter()
        .collect();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return;
    };

    let mut in_match = false;
    let mut hostname_done = false;
    for line in data.split('\n') {
        // 去掉注释与首尾空白
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut tok = line.split_whitespace();
        let Some(kw_raw) = tok.next() else { continue };
        let rest: Vec<&str> = tok.collect();
        let kw = kw_raw.to_ascii_lowercase();

        if kw == "host" {
            in_match = !rest.is_empty() && host_patterns_match(&rest, &o.host);
        } else if in_match && !rest.is_empty() {
            let val = rest[0];
            if kw == "hostname" && !hostname_done {
                o.host = val.to_string();
                hostname_done = true;
            } else if kw == "port" && o.port == 0 {
                o.port = atoi_like(val) as i32;
            } else if kw == "user" && o.user.is_empty() {
                o.user = val.to_string();
            } else if kw == "identityfile" && o.keyfile.is_none() {
                let kf = expand_tilde(val, home);
                o.keyfile = Some(identity_path(&kf, home));
            }
        }
    }
}

/// `~/.ssh/config` 里的主机别名（跳过通配/负向模式），供侧栏展示。
#[derive(Debug, Clone, PartialEq)]
pub struct SshHostAlias {
    pub alias: String,
    pub user: Option<String>,
    pub port: i32,
}

pub fn list_ssh_aliases_with_home(home: &Path) -> Vec<SshHostAlias> {
    let path: PathBuf = [home, Path::new(".ssh"), Path::new("config")]
        .iter()
        .collect();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };

    let mut out: Vec<SshHostAlias> = Vec::new();
    let mut blk_user: Option<String> = None;
    let mut blk_port: i32 = 0;
    let mut blk_aliases: Vec<String> = Vec::new();

    macro_rules! flush_block {
        () => {{
            let user = blk_user.take();
            let port = blk_port;
            for alias in blk_aliases.drain(..) {
                out.push(SshHostAlias {
                    alias,
                    user: user.clone(),
                    port,
                });
            }
        }};
    }

    for line in data.split('\n') {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let mut tok = line.split_whitespace();
        let Some(kw_raw) = tok.next() else { continue };
        let rest: Vec<&str> = tok.collect();
        let kw = kw_raw.to_ascii_lowercase();

        if kw == "host" && !rest.is_empty() {
            flush_block!();
            blk_port = 0;
            for p in rest {
                // 跳过通配与负向模式，只列可直接连接的别名
                if p.starts_with('!') || p.contains(['*', '?']) {
                    continue;
                }
                blk_aliases.push(p.to_string());
            }
        } else if kw == "user" && !rest.is_empty() {
            blk_user = Some(rest[0].to_string());
        } else if kw == "port" && !rest.is_empty() {
            blk_port = atoi_like(rest[0]) as i32;
        }
    }
    flush_block!();
    out
}

/* ---- PuTTY 会话文件（Linux 下 ~/.putty/sessions/<name>） ---- */

/// PuTTY 把会话名按 URL 百分号转义存成文件名。
pub fn session_filename_encode(name: &str) -> String {
    let mut s = String::with_capacity(name.len() + 8);
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b',') {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    s
}

/// 从 PuTTY 会话文件里补齐缺失字段；返回 true 表示成功读取。
pub fn load_putty_session_with_home(
    o: &mut ConnOptions,
    name: &str,
    home: &Path,
    warnings: &mut Vec<String>,
) -> bool {
    load_putty_session_from_dir(o, name, &putty_sessions_dir_with_home(home), warnings)
}

/// 从任意会话目录读一个 PuTTY 会话文件（导入时可以指向别的机器
/// 带来的会话目录，不必非得是本机 ~/.putty/sessions）。
pub fn load_putty_session_from_dir(
    o: &mut ConnOptions,
    name: &str,
    dir: &Path,
    warnings: &mut Vec<String>,
) -> bool {
    let path = dir.join(session_filename_encode(name));
    let Ok(data) = std::fs::read_to_string(&path) else {
        return false;
    };

    for line in data.split('\n') {
        let Some(eq) = line.find('=') else { continue };
        let key = line[..eq].trim();
        let val = line[eq + 1..].trim();

        if key == "HostName" && o.host.is_empty() {
            o.host = val.to_string();
        } else if key == "PortNumber" && o.port == 0 {
            o.port = atoi_like(val) as i32;
        } else if key == "UserName" && o.user.is_empty() {
            o.user = val.to_string();
        } else if key == "Protocol" {
            o.proto = val.to_ascii_lowercase();
        } else if key == "PortForwardings" && !val.is_empty() {
            // PuTTY 会话格式: L9000=127.0.0.1:9000,R2222=host:22,D1080=
            // 转换交给 forward::from_putty_value（2.3 的导出是它的逆运算，
            // 两边共用一套规则才叫双向互转；这里原来自己切了一遍）
            let internal = crate::backend::forward::from_putty_value(val);
            for r in crate::backend::forward::parse_rules(&internal) {
                o.forwards.push(format!("{}:{}", r.kind, r.spec));
            }
        } else if key == "PublicKeyFile" && o.keyfile.is_none() {
            o.keyfile = Some(val.to_string());
        } else if key == "ProxyType"
            && o.proxycmd.is_none()
            && !val.is_empty()
            && !val.eq_ignore_ascii_case("none")
        {
            warnings.push(trf!("会话 {name} 配置了代理 ({val})，代理设置不会自动生效", name = name, val = val));
        }
    }
    true
}

/* ---- 2.3 会话库导入导出：PuTTY 会话目录 ---- */

/// PuTTY 会话目录（Linux 下 ~/.putty/sessions）。
pub fn putty_sessions_dir_with_home(home: &Path) -> PathBuf {
    [home, Path::new(".putty"), Path::new("sessions")]
        .iter()
        .collect()
}

pub fn putty_sessions_dir() -> PathBuf {
    putty_sessions_dir_with_home(&crate::platform::paths::home())
}

/// 会话文件名的逆变换：%XX → 原字节。
/// PuTTY 编码是逐字节的，先收字节再一次性 UTF-8 解码；
/// 逐字符解码会把多字节序列拆坏。
pub fn session_filename_decode(enc: &str) -> String {
    let b = enc.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).ok();
            if let Some(v) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// 列出 PuTTY 会话目录下的会话名（已解码）。
pub fn list_putty_sessions_with_home(home: &Path) -> Vec<String> {
    let dir = putty_sessions_dir_with_home(home);
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| session_filename_decode(&e.file_name().to_string_lossy()))
        .filter(|n| !n.is_empty())
        .collect();
    names.sort();
    names
}

pub fn list_putty_sessions() -> Vec<String> {
    list_putty_sessions_with_home(&crate::platform::paths::home())
}

/// 写一个 PuTTY 会话文件（导出方向）。
/// 转发规则用 forward::to_putty_value 转；它表达不了的规则在
/// `skipped` 里回报给调用方——静默丢规则比导出失败更难查。
pub fn write_putty_session(
    dir: &Path,
    name: &str,
    o: &crate::args::ConnOptions,
) -> Result<Vec<String>, String> {
    std::fs::create_dir_all(dir).map_err(|e| trf!("创建 {v0} 失败: {e}", e = e, v0 = dir.display()))?;
    let path = dir.join(session_filename_encode(name));
    let rules: Vec<crate::backend::forward::FwdRule> = o
        .forwards
        .iter()
        .flat_map(|f| crate::backend::forward::parse_rules(f))
        .collect();
    let (pf, skipped) = crate::backend::forward::to_putty_value(&rules);

    let mut out = String::new();
    let mut put = |k: &str, v: &str| {
        out.push_str(k);
        out.push('=');
        out.push_str(v);
        out.push('\n');
    };
    if o.proto == "serial" {
        // PuTTY 串口会话：设备路径与串口参数分字段
        put("Protocol", "serial");
        put("SerialPort", &o.host);
        put("SerialLine", &format!("{}", o.serial_baud));
        put("SerialDataBits", &format!("{}", o.serial_data));
        put(
            "SerialStopBits",
            if o.serial_stop == 2 { "2" } else { "1" },
        );
        put(
            "Parity",
            match o.serial_parity {
                'e' => "Even",
                'o' => "Odd",
                'm' => "Mark",
                's' => "Space",
                _ => "None",
            },
        );
    } else {
        put("HostName", &o.host);
        put("PortNumber", &format!("{}", o.port));
        put("Protocol", &o.proto);
    }
    if !o.user.is_empty() {
        put("UserName", &o.user);
    }
    if let Some(k) = o.keyfile.as_deref().filter(|k| !k.is_empty()) {
        put("PublicKeyFile", k);
    }
    if !pf.is_empty() {
        put("PortForwardings", &pf);
    }
    std::fs::write(&path, out).map_err(|e| trf!("写入 {v0} 失败: {e}", e = e, v0 = path.display()))?;
    Ok(skipped)
}

/* ---- 便捷封装：用真实主目录 ---- */

pub fn resolve_ssh_config_alias(o: &mut ConnOptions) {
    resolve_ssh_config_alias_with_home(o, &crate::platform::paths::home());
}

pub fn list_ssh_aliases() -> Vec<SshHostAlias> {
    list_ssh_aliases_with_home(&crate::platform::paths::home())
}

pub fn load_putty_session(
    o: &mut ConnOptions,
    name: &str,
    warnings: &mut Vec<String>,
) -> bool {
    load_putty_session_with_home(o, name, &crate::platform::paths::home(), warnings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::args::ConnOptions;
    use crate::util::testutil::temp_home;
    use std::fs;

    #[test]
    fn fnmatch_basic() {
        assert!(fnmatch("abc", "abc"));
        assert!(!fnmatch("abc", "abd"));
        assert!(fnmatch("a*c", "abc"));
        assert!(fnmatch("a*c", "ac"));
        assert!(fnmatch("a?c", "axc"));
        assert!(!fnmatch("a?c", "ac"));
        assert!(fnmatch("*.example.com", "a.example.com"));
        assert!(fnmatch("[ab]c", "ac"));
        assert!(!fnmatch("[ab]c", "bc-x"));
        assert!(fnmatch("[a-z]x", "mx"));
        assert!(!fnmatch("[a-z]x", "Mx"));
        assert!(fnmatch("[!a]x", "bx"));
        assert!(!fnmatch("[!a]x", "ax"));
        assert!(fnmatch("[^a]x", "bx"));
        // 未闭合字符类按字面处理
        assert!(fnmatch("[ab", "[ab"));
        assert!(!fnmatch("[ab", "a"));
    }

    #[test]
    fn patterns_match_order() {
        assert!(host_patterns_match(&["*"], "anything"));
        assert!(host_patterns_match(&["*.corp", "jumphost"], "a.corp"));
        // 正向命中后出现负向命中 → 否决
        assert!(!host_patterns_match(&["*", "!bad"], "bad"));
        assert!(host_patterns_match(&["*", "!bad"], "good"));
        assert!(!host_patterns_match(&["!bad", "*"], "bad"));
        // 只有负向不构成匹配
        assert!(!host_patterns_match(&["!bad"], "good"));
    }

    #[test]
    fn session_filename() {
        assert_eq!(session_filename_encode("My Server"), "My%20Server");
        assert_eq!(session_filename_encode("a.b-c_d,e"), "a.b-c_d,e");
        assert_eq!(session_filename_encode("中"), "%E4%B8%AD");
        assert_eq!(session_filename_encode("a/b"), "a%2Fb");
        assert_eq!(session_filename_encode("100%"), "100%25");
        assert_eq!(session_filename_encode("a:b@h"), "a%3Ab%40h");
    }

    /// `~` 展开：**home 与其余部分之间用平台的分隔符**。
    ///
    /// ⚠️ 为什么要单独测：Windows 上 home 是 `C:\Users\bob`，展开 `~/.ssh/id_rsa`
    ///    必须得到 `C:\Users\bob\.ssh/id_rsa`（边界是 `\`）⇒ 跟同一份代码里
    ///    `Path::join` 的结果一致。之前这里是纯 POSIX 拼接
    ///    （`home + "/" + rest`）⇒ Windows 上得到 `C:\Users\bob/.ssh/id_rsa`
    ///    ⇒ Windows CI 的 `args::tests::tilde_expand_keyfile` 直接炸。
    ///
    /// ⚠️ 这里**把「是不是 Windows」当参数传进来**⇒ Linux 上也能把
    ///    Windows 的行为测到，不用等 Windows CI 才发现自己写错了。
    #[test]
    fn 波浪号展开的分隔符随平台() {
        let win_home = Path::new("C:\\Users\\bob");
        // Windows：边界是反斜杠，其余部分原样保留
        assert_eq!(
            expand_tilde_for("~/.ssh/id_rsa", win_home, true),
            "C:\\Users\\bob\\.ssh/id_rsa"
        );
        // 只有 ~ 没有后续 ⇒ 就是 home 本身
        assert_eq!(expand_tilde_for("~", win_home, true), "C:\\Users\\bob");
        // ~user/x 不展开（契约）
        assert_eq!(expand_tilde_for("~someone/x", win_home, true), "~someone/x");
        // 中间的 ~ 不展开
        assert_eq!(expand_tilde_for("/a/~/b", win_home, true), "/a/~/b");
        // Unix：边界是正斜杠
        let nix_home = Path::new("/home/bob");
        assert_eq!(
            expand_tilde_for("~/.ssh/id_rsa", nix_home, false),
            "/home/bob/.ssh/id_rsa"
        );
        // home 末尾自带分隔符 ⇒ 不重复加
        assert_eq!(
            expand_tilde_for("~/.ssh/id_rsa", Path::new("C:\\Users\\bob\\"), true),
            "C:\\Users\\bob\\.ssh/id_rsa"
        );
        // 与 `Path::join` 的结果一致（Windows 上 `Path::join` 用的也是 `\`）
        // ⚠️ 只能在 Windows 上断言：Linux 的 `Path::join` 用的是 `/`，
        //    拿 Windows 形状的路径去比没有意义。
        #[cfg(windows)]
        assert_eq!(
            expand_tilde_for("~/.ssh/id_rsa", win_home, true),
            win_home.join(".ssh/id_rsa").to_str().unwrap()
        );
        // 真正的入口用的就是这个平台判断 ⇒ 别让它跟 `expand_tilde_for` 脱节
        assert_eq!(
            expand_tilde("~/.ssh/id_rsa", win_home),
            expand_tilde_for("~/.ssh/id_rsa", win_home, cfg!(windows))
        );
    }

    /// `IdentityFile` 的绝对/相对判定。
    ///
    /// ⚠️ Windows 上 home 展开出来是**盘符**开头（`C:\Users\bob\...`），
    ///    只判 `starts_with('/')` 会把绝对路径误当成相对路径、再补一层
    ///    `~/.ssh/` ⇒ 密钥路径整个错掉。
    /// ⚠️ 这里用的是**不分平台**的形状判定 ⇒ Linux 上也能把 Windows 的
    ///    盘符形态测到（不用等 Windows CI）。
    #[test]
    fn identity_路径的绝对与相对() {
        let home = Path::new("/home/bob");
        // 绝对路径原样用
        assert_eq!(identity_path("/keys/id_rsa", home), "/keys/id_rsa");
        // Windows 的盘符形态也是绝对路径 ⇒ 不能被补一层 ~/.ssh/
        assert_eq!(
            identity_path("C:\\Users\\bob\\.ssh/id_rsa", home),
            "C:\\Users\\bob\\.ssh/id_rsa"
        );
        // UNC 也是
        assert_eq!(
            identity_path("\\\\server\\share\\id_rsa", home),
            "\\\\server\\share\\id_rsa"
        );
        // 相对路径补到 ~/.ssh/ 下
        assert_eq!(
            identity_path("rel_key", home),
            home.join(".ssh/rel_key").to_str().unwrap()
        );
    }

    /// 文件名编码/解码互逆：PuTTY 用 %XX 编码会话名，中文名尤其容易坏
    #[test]
    fn session_filename_roundtrip() {
        for n in ["My Server", "堡垒机", "a+b=c", "100%", "web-01"] {
            assert_eq!(session_filename_decode(&session_filename_encode(n)), n, "{n}");
        }
        // 裸 % 与不完整转义要能原样还原，不能 panic
        assert_eq!(session_filename_decode("100%"), "100%");
        assert_eq!(session_filename_decode("%zz"), "%zz");
        assert_eq!(session_filename_decode("%4"), "%4");
    }

    /// 导入→导出→再导入：转发规则应当回到等价状态（双向互转的闭环）
    #[test]
    fn putty_export_import_roundtrip() {
        let home = crate::util::testutil::temp_home("pp_cfg_export");
        let dir = putty_sessions_dir_with_home(home.as_path());
        std::fs::create_dir_all(&dir).unwrap();

        let mut o = crate::args::ConnOptions {
            host: "10.0.0.9".into(),
            port: 2222,
            user: "ops".into(),
            forwards: vec!["L:9000:127.0.0.1:9000".into(), "R:2222:app:22".into()],
            ..Default::default()
        };
        let skipped = write_putty_session(&dir, "堡垒机", &o).unwrap();
        assert!(skipped.is_empty(), "规则都该表达得了：{skipped:?}");

        let mut back = crate::args::ConnOptions::default();
        let mut warnings = Vec::new();
        assert!(load_putty_session_from_dir(&mut back, "堡垒机", &dir, &mut warnings));
        assert_eq!(back.host, o.host);
        assert_eq!(back.port, o.port);
        assert_eq!(back.user, o.user);
        assert_eq!(back.forwards, o.forwards, "转发规则应当往返一致");

        // 列目录也能列出来
        assert_eq!(list_putty_sessions_with_home(home.as_path()), vec!["堡垒机"]);
    }

    /// 串口条目的导出：设备路径与串口参数分字段写，不能当主机名写
    #[test]
    fn putty_export_serial_session() {
        let home = crate::util::testutil::temp_home("pp_cfg_serial");
        let dir = putty_sessions_dir_with_home(home.as_path());
        std::fs::create_dir_all(&dir).unwrap();
        let o = crate::args::ConnOptions {
            proto: "serial".into(),
            host: "/dev/ttyUSB0".into(),
            serial_baud: 9600,
            serial_data: 7,
            serial_parity: 'e',
            serial_stop: 2,
            ..Default::default()
        };
        write_putty_session(&dir, "switch", &o).unwrap();
        let text = std::fs::read_to_string(dir.join("switch")).unwrap();
        assert!(text.contains("Protocol=serial"), "{text}");
        assert!(text.contains("SerialPort=/dev/ttyUSB0"), "{text}");
        assert!(text.contains("SerialLine=9600"), "{text}");
        assert!(!text.contains("HostName="), "串口不该写 HostName：{text}");
    }

    #[test]
    fn putty_session_load() {
        let home = temp_home("session");
        fs::create_dir_all(home.join(".putty/sessions")).unwrap();
        fs::write(
            home.join(".putty/sessions/My%20Server"),
            "HostName=192.168.1.10\nPortNumber=2222\nUserName=admin\nProtocol=ssh\n\
             PortForwardings=L9000=127.0.0.1:9000,R2222=host:22,D1080=\n\
             PublicKeyFile=/home/u/key\nProxyType=http\n",
        )
        .unwrap();

        let mut o = ConnOptions::default();
        let mut w = Vec::new();
        assert!(load_putty_session_with_home(&mut o, "My Server", &home, &mut w));
        assert_eq!(o.host, "192.168.1.10");
        assert_eq!(o.port, 2222);
        assert_eq!(o.user, "admin");
        assert_eq!(o.proto, "ssh");
        assert_eq!(
            o.forwards,
            vec![
                "L:9000:127.0.0.1:9000".to_string(),
                "R:2222:host:22".to_string(),
                "D:1080".to_string(),
            ]
        );
        assert_eq!(o.keyfile.as_deref(), Some("/home/u/key"));
        assert_eq!(w.len(), 1, "应产生代理警告");
        assert!(w[0].contains("代理"));

        // 已有字段不被覆盖
        let mut o2 = ConnOptions::default();
        o2.host = "cli.example.com".into();
        o2.port = 100;
        o2.user = "cli".into();
        o2.keyfile = Some("/k".into());
        let mut w2 = Vec::new();
        assert!(load_putty_session_with_home(&mut o2, "My Server", &home, &mut w2));
        assert_eq!(o2.host, "cli.example.com");
        assert_eq!(o2.port, 100);
        assert_eq!(o2.user, "cli");
        assert_eq!(o2.keyfile.as_deref(), Some("/k"));

        // 不存在的会话
        let mut o3 = ConnOptions::default();
        assert!(!load_putty_session_with_home(&mut o3, "nope", &home, &mut Vec::new()));
    }

    #[test]
    fn ssh_alias_resolution() {
        let home = temp_home("alias");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::write(
            home.join(".ssh/config"),
            "# 注释\nHost jump *.corp\n  HostName 10.0.0.1\n  Port 2222\n  User ops\n  IdentityFile ~/.ssh/jump_key\n\nHost other\n  HostName 1.2.3.4\n",
        )
        .unwrap();

        let mut o = ConnOptions::default();
        o.host = "a.corp".into();
        resolve_ssh_config_alias_with_home(&mut o, &home);
        assert_eq!(o.host, "10.0.0.1");
        assert_eq!(o.port, 2222);
        assert_eq!(o.user, "ops");
        assert_eq!(o.keyfile.as_deref(), Some(home.join(".ssh/jump_key").to_str().unwrap()));

        // 相对路径 IdentityFile → ~/.ssh/ 下
        fs::write(
            home.join(".ssh/config"),
            "Host x\n  IdentityFile rel_key\n",
        )
        .unwrap();
        let mut o2 = ConnOptions::default();
        o2.host = "x".into();
        resolve_ssh_config_alias_with_home(&mut o2, &home);
        assert_eq!(
            o2.keyfile.as_deref(),
            Some(home.join(".ssh/rel_key").to_str().unwrap())
        );

        // 负向模式否决
        fs::write(
            home.join(".ssh/config"),
            "Host * !excluded\n  HostName 10.9.9.9\n",
        )
        .unwrap();
        let mut o3 = ConnOptions::default();
        o3.host = "excluded".into();
        resolve_ssh_config_alias_with_home(&mut o3, &home);
        assert_eq!(o3.host, "excluded", "负向否决后不应改写 host");
    }

    #[test]
    fn alias_listing() {
        let home = temp_home("listalias");
        fs::create_dir_all(home.join(".ssh")).unwrap();
        fs::write(
            home.join(".ssh/config"),
            "Host web db\n  User deploy\n  Port 2222\n\nHost *.wild !neg jumphost\nHost plain\n",
        )
        .unwrap();
        let list = list_ssh_aliases_with_home(&home);
        let names: Vec<&str> = list.iter().map(|a| a.alias.as_str()).collect();
        assert_eq!(names, vec!["web", "db", "jumphost", "plain"]);
        assert_eq!(list[0].user.as_deref(), Some("deploy"));
        assert_eq!(list[0].port, 2222);
        // 后续块 flush 后 user/port 复位
        assert_eq!(list[2].user, None);
        assert_eq!(list[2].port, 0);
    }
}
