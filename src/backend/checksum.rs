//! 传输完成后的 SHA-1 校验（3.5）。
//!
//! 本地用 SHA-1（`sha1` 本来就是直接依赖，给凭据派生 HMAC 用的，顺手）。
//! 远端按 `sha1sum` → `shasum` → `openssl dgst -sha1` 的顺序试，**缓存
//! 试成功的那种**，一个文件只多花一次往返。
//!
//! 一个都没有就明确说"没有可用的校验和工具，未校验"——宁可承认没校验，
//! 也不能把没校验说成校验过了。

use std::path::Path;

use sha1::{Digest, Sha1};

use super::conn::SftpBackend;
use super::BackendResult;

/// SHA-1 的十六进制长度。
const HEX_LEN: usize = 40;

/// 本地文件 SHA-1（小文件一次性读，大文件分块读）。
pub fn sha1_file(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha1::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex(&h.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// shell 单引号转义。远端路径来自目录列表，用户起的名字什么字符都可能有。
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// 从 `sha1sum` / `openssl dgst` 的输出里摘出摘要。
///
/// `sha1sum f` → `"<hex>  f"`；`openssl dgst -sha1 f` →
/// `"SHA1(f)= <hex>"`；BSD `shasum` 同 sha1sum。倒着扫第一个像摘要的
/// token，最不挑输出格式。
pub fn parse_sha1(out: &str) -> Option<String> {
    out.split_whitespace()
        .rev()
        .find(|t| t.len() == HEX_LEN && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|t| t.to_ascii_lowercase())
}

/// 远端可用的 SHA-1 工具。`Mutex<Option<&'static str>>`：
/// None = 还没试过，Some("") = 试过了都没有。
const NO_TOOL: &str = "";

/// 远端文件的 SHA-1。
///
/// Err 的文案就是给用户看的：区分"没有工具"和"工具跑失败"，
/// 前者不是用户的错，不该报成传输失败。
pub async fn remote_sha1(be: &SftpBackend, path: &str) -> BackendResult<String> {
    // 缓存里已有可用工具就直接用，别每个文件都试一遍
    // （必须先把锁里的值取出来再 await：MutexGuard 不是 Send）
    let cached = *be.sha1_tool.lock().unwrap();
    if let Some(t) = cached {
        if t.is_empty() {
            return Err(no_tool_err());
        }
        return run_tool(be, t, path).await;
    }
    for tool in ["sha1sum", "shasum", "openssl dgst -sha1"] {
        if let Ok(h) = run_tool(be, tool, path).await {
            *be.sha1_tool.lock().unwrap() = Some(tool);
            return Ok(h);
        }
    }
    *be.sha1_tool.lock().unwrap() = Some(NO_TOOL);
    Err(no_tool_err())
}

fn no_tool_err() -> super::BackendError {
    super::BackendError(
        "服务器上没找到可用的 SHA-1 工具（试过 sha1sum / shasum / openssh dgst）".into(),
    )
}

async fn run_tool(be: &SftpBackend, tool: &str, path: &str) -> BackendResult<String> {
    let cmd = format!("{} -- {}", tool, sh_quote(path));
    // openssh 的 dgst 不认 "--"，用空格分隔；两种写法都试一下成本很低
    let out = match be.exec(&cmd, 15_000, 4096).await {
        Ok(o) => o,
        Err(_) => {
            let cmd2 = format!("{} {}", tool, sh_quote(path));
            be.exec(&cmd2, 15_000, 4096).await?
        }
    };
    parse_sha1(&out).ok_or_else(|| super::BackendError(format!("解析 {tool} 输出失败")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 本地哈希是标准_sha1() {
        let p = std::env::temp_dir().join("pp_checksum_test.bin");
        std::fs::write(&p, b"abc").unwrap();
        assert_eq!(
            sha1_file(&p).unwrap(),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn 摘摘要认得三种格式() {
        let hex = "a9993e364706816aba3e25717850c26c9cd0d89d";
        // sha1sum / shasum
        assert_eq!(
            parse_sha1(&format!("{hex}  /tmp/a.bin\n")),
            Some(hex.to_string())
        );
        // openssh dgst
        assert_eq!(
            parse_sha1(&format!("SHA1(/tmp/a.bin)= {hex}\n")),
            Some(hex.to_string())
        );
        // 大写也认（统一转小写再比）
        assert_eq!(parse_sha1(&format!("{}\n", hex.to_uppercase())), Some(hex.into()));
    }

    #[test]
    fn 认不出就不是摘要() {
        assert_eq!(parse_sha1("sha1sum: /tmp/x: No such file or directory\n"), None);
        assert_eq!(parse_sha1(""), None);
        assert_eq!(parse_sha1("da39a3ee5e6b4b0d3255bfef95601890afd80709"), Some("da39a3ee5e6b4b0d3255bfef95601890afd80709".into()));
        // 长度不对（32 位是 MD5，不能拿来比 SHA-1）
        assert_eq!(parse_sha1("d41d8cd98f00b204e9800998ecf8427e x"), None);
    }

    #[test]
    fn 路径引号转义() {
        assert_eq!(sh_quote("/tmp/a.bin"), "'/tmp/a.bin'");
        assert_eq!(sh_quote("/tmp/it's"), r"'/tmp/it'\''s'");
        assert_eq!(sh_quote("a b; rm -rf /"), "'a b; rm -rf /'");
        // 换行也不能逃出引号
        assert_eq!(sh_quote("a\nb"), "'a\nb'");
    }
}
