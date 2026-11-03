//! `~/.ssh/known_hosts` 校验（替代 libssh 的 ssh_session_is_known_server）。
//!
//! 语义：主机已有条目且密钥一致 → 通过；条目存在但密钥不同 → 拒绝（密钥已变化）；
//! 首次见到 → 追加写入（与 `ssh -o StrictHostKeyChecking=accept-new` 一致）。
//! 支持 `|1|salt|hash` 哈希条目、`[host]:port` 非默认端口、`@revoked` 标记；
//! `@cert-authority` 行只对证书生效，对普通主机密钥不参与比较。

use std::path::Path;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use russh::keys::ssh_key::PublicKey;
use sha1::Sha1;
use crate::i18n::tr;
use crate::trf;

type HmacSha1 = Hmac<Sha1>;

/// 主机密钥**变了**的提示。
///
/// ⚠️ 原来是 `const` —— 但 const 是**编译期求值**，不能调 `tr()`
///    ⇒ 英文界面里这句还是中文。改成函数，运行时翻译。
pub fn changed_msg() -> &'static str {
    tr("主机密钥校验失败（密钥已变化）。\n\
       若确定环境安全，可删除 ~/.ssh/known_hosts 中相关条目后重试。")
}

/// 主机密钥**被吊销**的提示（同 [`changed_msg`]：const 翻不了）。
pub fn revoked_msg() -> &'static str {
    tr("主机密钥校验失败（该密钥已被标记吊销）。\n\
       若确定环境安全，可删除 ~/.ssh/known_hosts 中相关条目后重试。")
}

#[derive(Debug)]
struct KhEntry {
    revoked: bool,
    cert_authority: bool,
    hosts: String,
    keytype: String,
    key_b64: String,
}

fn parse_file(data: &str) -> Vec<KhEntry> {
    data.lines()
        .filter_map(|line| {
            let mut rest = line.trim();
            if rest.is_empty() || rest.starts_with('#') {
                return None;
            }
            let mut revoked = false;
            let mut cert_authority = false;
            loop {
                if let Some(r) = rest.strip_prefix("@revoked ") {
                    revoked = true;
                    rest = r;
                } else if let Some(r) = rest.strip_prefix("@cert-authority ") {
                    cert_authority = true;
                    rest = r;
                } else {
                    break;
                }
            }
            let mut it = rest.split_whitespace();
            Some(KhEntry {
                hosts: it.next()?.to_string(),
                keytype: it.next()?.to_string(),
                key_b64: it.next()?.to_string(),
                revoked,
                cert_authority,
            })
        })
        .collect()
}

/// host 字段（逗号分隔模式）是否命中目标；目标已含 "[host]:port" 形式。
/// 按 OpenSSH match_hostname 语义只支持 `*` 和 `?`——`[`/`]` 是字面量
/// （否则 "[host]:port" 会被误当字符类解析）。
fn hosts_match(hosts_field: &str, target: &str) -> bool {
    hosts_field.split(',').any(|p| fnmatch_star_q(p, target))
}

/// 只有 `*` 和 `?` 的通配匹配（OpenSSH match_pattern 子集）。
fn fnmatch_star_q(pat: &str, text: &str) -> bool {
    fn m(p: &[u8], t: &[u8]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some(b'*') => m(&p[1..], t) || (!t.is_empty() && m(p, &t[1..])),
            Some(b'?') => !t.is_empty() && m(&p[1..], &t[1..]),
            Some(&c) => t.first() == Some(&c) && m(&p[1..], &t[1..]),
        }
    }
    m(pat.as_bytes(), text.as_bytes())
}

/// `|1|salt_b64|hash_b64` 哈希条目匹配（HMAC-SHA1）。
fn hashed_hosts_match(field: &str, target: &str) -> bool {
    let Some(rest) = field.strip_prefix("|1|") else {
        return false;
    };
    let Some((salt, hash)) = rest.split_once('|') else {
        return false;
    };
    let (Ok(salt), Ok(hash)) = (B64.decode(salt), B64.decode(hash)) else {
        return false;
    };
    let Ok(mut mac) = HmacSha1::new_from_slice(&salt) else {
        return false;
    };
    mac.update(target.as_bytes());
    let computed = mac.finalize().into_bytes();
    computed.as_slice() == hash.as_slice()
}

/// 把公钥编码成 known_hosts 行使用的 (keytype, base64)。
fn key_wire(key: &PublicKey) -> Option<(String, String)> {
    let mut buf = [0u8; 8192];
    let s = key.encode_openssh(&mut buf).ok()?;
    let mut it = s.split(' ');
    Some((it.next()?.to_string(), it.next()?.to_string()))
}

#[derive(Debug, PartialEq)]
pub enum HostKeyCheck {
    /// 已有条目匹配
    Match,
    /// 首次见到，已追加写入 known_hosts
    Learned,
}

/// 校验主机密钥；未见过时追加（accept-new）。
pub fn verify_and_learn(
    host: &str,
    port: i32,
    key: &PublicKey,
    kh_path: &Path,
) -> Result<HostKeyCheck, String> {
    let target = if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    };
    let (kt, b64) = key_wire(key).ok_or(tr("无法编码主机公钥"))?;

    let data = match std::fs::read_to_string(kh_path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(trf!("主机密钥校验出错: {e}", e = e)),
    };
    let entries = parse_file(&data);

    let mut found_same = false;
    let mut found_diff = false;
    for e in &entries {
        if e.cert_authority {
            continue;
        }
        let hit = if e.hosts.starts_with("|1|") {
            hashed_hosts_match(&e.hosts, &target)
        } else {
            hosts_match(&e.hosts, &target)
        };
        if !hit {
            continue;
        }
        if e.keytype == kt && e.key_b64 == b64 {
            if e.revoked {
                return Err(revoked_msg().into());
            }
            found_same = true;
        } else {
            found_diff = true;
        }
    }
    if found_same {
        return Ok(HostKeyCheck::Match);
    }
    if found_diff {
        return Err(changed_msg().into());
    }

    // 首次见到：追加写入
    let line = format!("{target} {kt} {b64}\n");
    std::fs::write(kh_path, format!("{data}{line}")).map_err(|e| {
        trf!("无法写入 {v0}: {e}（可手动把主机条目加入 known_hosts）", e = e, v0 = kh_path.display())
    })?;
    Ok(HostKeyCheck::Learned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testutil::temp_home;
    use russh::keys::ssh_key::PublicKey;

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPMCCdPrmsLIqzcGDbvw1dOIgIuWUOcyVHhIHsjtBpBj";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIF/1VHPPJtcVHheaOf+nOSV395/xDkrUlWUciEHnK6jK";

    fn key_a() -> PublicKey {
        PublicKey::from_openssh(KEY_A).unwrap()
    }
    fn key_b() -> PublicKey {
        PublicKey::from_openssh(KEY_B).unwrap()
    }

    fn kh(dir: &Path) -> std::path::PathBuf {
        dir.join("known_hosts")
    }

    #[test]
    fn accept_new_flow() {
        let dir = temp_home("kh1");
        // 首次：Learned 并写入
        assert_eq!(
            verify_and_learn("srv", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Learned
        );
        let data = std::fs::read_to_string(kh(&dir)).unwrap();
        assert_eq!(data, format!("{KEY_A}\n").replace("ssh-ed25519 ", "srv ssh-ed25519 "));
        // 第二次：Match
        assert_eq!(
            verify_and_learn("srv", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Match
        );
        // 密钥变化：拒绝
        let err = verify_and_learn("srv", 22, &key_b(), &kh(&dir)).unwrap_err();
        assert!(err.contains("密钥已变化"), "{err}");
    }

    #[test]
    fn nondefault_port_form() {
        let dir = temp_home("kh2");
        // 2222 端口写为 [srv]:2222；不会与 22 端口条目混淆
        verify_and_learn("srv", 2222, &key_a(), &kh(&dir)).unwrap();
        let data = std::fs::read_to_string(kh(&dir)).unwrap();
        assert!(data.contains("[srv]:2222"));
        assert_eq!(
            verify_and_learn("srv", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Learned,
            "22 端口应视为首次"
        );
    }

    #[test]
    fn wildcard_and_multi_hosts() {
        let dir = temp_home("kh3");
        std::fs::write(kh(&dir), format!("a.example,b.example {KEY_A}\n")).unwrap();
        assert_eq!(
            verify_and_learn("b.example", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Match
        );
        std::fs::write(kh(&dir), format!("*.example.com {KEY_A}\n")).unwrap();
        assert_eq!(
            verify_and_learn("srv.example.com", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Match
        );
    }

    #[test]
    fn bracketed_port_is_literal_not_charclass() {
        let dir = temp_home("kh3b");
        // "[host]:port" 字段不能被当字符类解析（曾导致条目永远匹配不上）
        std::fs::write(kh(&dir), format!("[192.0.2.1]:2223 {KEY_A}\n")).unwrap();
        assert_eq!(
            verify_and_learn("192.0.2.1", 2223, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Match
        );
        // 同名但不同端口视作不同主机
        assert_eq!(
            verify_and_learn("192.0.2.1", 2224, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Learned
        );
        // 密钥变化必须拒绝
        assert!(verify_and_learn("192.0.2.1", 2223, &key_b(), &kh(&dir)).is_err());
    }

    #[test]
    fn hashed_entry() {
        let dir = temp_home("kh4");
        // 构造 |1|salt|hash 条目：salt 固定，hash = HMAC-SHA1(salt, "srv")
        let salt = [7u8; 16];
        let mut mac = HmacSha1::new_from_slice(&salt).unwrap();
        mac.update(b"srv");
        let tag = mac.finalize().into_bytes();
        let line = format!("|1|{}|{} {KEY_A}\n",
            B64.encode(salt),
            B64.encode(tag)
        );
        std::fs::write(kh(&dir), line).unwrap();
        assert_eq!(
            verify_and_learn("srv", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Match
        );
        // 哈希条目密钥不同 → 视为变化
        assert!(verify_and_learn("srv", 22, &key_b(), &kh(&dir)).is_err());
    }

    #[test]
    fn revoked_rejected() {
        let dir = temp_home("kh5");
        std::fs::write(kh(&dir), format!("@revoked srv {KEY_A}\n")).unwrap();
        let err = verify_and_learn("srv", 22, &key_a(), &kh(&dir)).unwrap_err();
        assert!(err.contains("吊销"), "{err}");
    }

    #[test]
    fn cert_authority_ignored_for_plain_keys() {
        let dir = temp_home("kh6");
        // CA 行密钥不同也不应触发"密钥已变化"
        std::fs::write(kh(&dir), format!("@cert-authority srv {KEY_B}\n")).unwrap();
        assert_eq!(
            verify_and_learn("srv", 22, &key_a(), &kh(&dir)).unwrap(),
            HostKeyCheck::Learned
        );
    }
}
