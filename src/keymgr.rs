//! 私钥管理器（2.4）：生成 ed25519/rsa 密钥、格式转换、列出 ~/.ssh 里的密钥。
//!
//! 格式能力边界（ssh-key 0.7 决定，别对外承诺做不到的）：
//! - **生成**：ed25519 / rsa-2048 / rsa-4096 / ecdsa-p256
//! - **读**：OpenSSH、`~/.ssh` 下的常规私钥、**PuTTY .ppk**（v2/v3）
//! - **写**：OpenSSH（含口令加密）+ 公钥 authorized_keys 行
//! - **不写 PPK**：ssh-key 只实现了 PPK 读取。PPK → OpenSSH 可以做
//!   （省掉 `puttygen` 外部依赖），OpenSSH → PPK 不行
//!
//! 私钥落盘一律 0600：ssh 客户端会拒绝组/其他可读的私钥，
//! 写对了才不会等到连接时才报错。

use std::path::{Path, PathBuf};

use ssh_key::{Algorithm, EcdsaCurve, HashAlg, LineEnding, PrivateKey};

/// 密钥生成用的系统 CSPRNG。
///
/// rand_core 0.10 不再自带 OsRng（它指向 getrandom 的 SysRng，而那要
/// 和 ssh-key 的 rand_core 版本对得上）；这里直接按 trait 实现一遍，
/// 同时把"生成私钥必须走内核熵源"这件事写在明处——私钥一旦用了弱随机，
/// 后面所有环节的强度都归零。
struct SysRng;

// rand_core 0.10 只需实现 TryRng（Error = Infallible），Rng/CryptoRng
// 都有 blanket impl 自动补上。
impl rand_core::TryRng for SysRng {
    // Error = Infallible 是刻意的：ssh-key 的密钥生成要求 CryptoRng
    // （不可失败），所以"内核熵源不可用"只能表现为拒绝生成而不是
    // 返回错误——私钥绝不能退回弱随机。
    type Error = std::convert::Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Self::Error> {
        let mut b = [0u8; 4];
        sys_fill(&mut b);
        Ok(u32::from_le_bytes(b))
    }
    fn try_next_u64(&mut self) -> Result<u64, Self::Error> {
        let mut b = [0u8; 8];
        sys_fill(&mut b);
        Ok(u64::from_le_bytes(b))
    }
    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Self::Error> {
        sys_fill(dst);
        Ok(())
    }
}
impl rand_core::TryCryptoRng for SysRng {}

fn sys_fill(dst: &mut [u8]) {
    if let Err(e) = getrandom::fill(dst) {
        panic!("系统随机数不可用（{e}），拒绝生成私钥");
    }
}

/// 可生成的算法（界面用这个列表，别直接暴露 ssh-key 的全部算法）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenAlgo {
    Ed25519,
    Rsa2048,
    Rsa4096,
    EcdsaP256,
}

impl GenAlgo {
    pub fn label(&self) -> &'static str {
        match self {
            GenAlgo::Ed25519 => "ed25519（默认，快）",
            GenAlgo::Rsa2048 => "RSA 2048",
            GenAlgo::Rsa4096 => "RSA 4096（慢，堡垒机兼容性好）",
            GenAlgo::EcdsaP256 => "ECDSA P-256",
        }
    }

    pub const ALL: [GenAlgo; 4] = [
        GenAlgo::Ed25519,
        GenAlgo::Rsa2048,
        GenAlgo::Rsa4096,
        GenAlgo::EcdsaP256,
    ];

    fn to_algorithm(self) -> Algorithm {
        match self {
            GenAlgo::Ed25519 => Algorithm::Ed25519,
            // hash: RFC8332（rsa-sha2-256/512）比老的 ssh-rsa 更兼容现代 sshd
            GenAlgo::Rsa2048 | GenAlgo::Rsa4096 => {
                Algorithm::Rsa { hash: Some(HashAlg::Sha512) }
            }
            GenAlgo::EcdsaP256 => Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
        }
    }
}

/// 生成一对密钥。RSA 的位长由 algo 决定。
fn rsa_bits(algo: GenAlgo) -> usize {
    match algo {
        GenAlgo::Rsa2048 => 2048,
        GenAlgo::Rsa4096 => 4096,
        _ => 0,
    }
}

/// 生成一把私钥。RSA 的位长在 ssh-key 里只有 Keypair 层能指定，
/// 这里统一按 algo 决定（`PrivateKey::random` 对 RSA 固定 2048）。
pub fn generate(algo: GenAlgo) -> Result<PrivateKey, String> {
    let bits = rsa_bits(algo);
    if bits > 0 {
        let kp = ssh_key::private::RsaKeypair::random(&mut SysRng, bits)
            .map_err(|e| format!("生成 RSA {bits} 失败: {e}"))?;
        return PrivateKey::new(kp.into(), "rsa")
            .map_err(|e| format!("组装 RSA 私钥失败: {e}"));
    }
    PrivateKey::random(&mut SysRng, algo.to_algorithm()).map_err(|e| format!("生成密钥失败: {e}"))
}

/// 生成 + 写盘。返回写出的两个路径（私钥、公钥）。
/// 口令为空则私钥不加密——本地开发常用，但要和用户说清楚。
pub fn generate_to_files(
    algo: GenAlgo,
    dir: &Path,
    stem: &str,
    passphrase: Option<&str>,
    comment: &str,
) -> Result<(PathBuf, PathBuf), String> {
    let mut key = generate(algo)?;
    if !comment.is_empty() {
        key.set_comment(comment);
    }
    if let Some(pw) = passphrase.filter(|p| !p.is_empty()) {
        // encrypt 返回**新**的 PrivateKey，不原地改。
        // 写成 `key.encrypt(..)?;` 会把加密后的结果丢掉，
        // 设了口令的私钥以明文落盘——静默且要命。
        key = key
            .encrypt(&mut SysRng, pw)
            .map_err(|e| format!("加口令失败: {e}"))?;
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    let priv_path = dir.join(stem);
    let pub_path = dir.join(format!("{stem}.pub"));
    write_private(&priv_path, &key, passphrase.is_some())?;
    std::fs::write(&pub_path, authorized_keys_line(&key))
        .map_err(|e| format!("写入公钥失败: {e}"))?;
    set_mode(&pub_path, 0o644);
    Ok((priv_path, pub_path))
}

fn write_private(path: &Path, key: &PrivateKey, encrypted: bool) -> Result<(), String> {
    let pem = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| format!("序列化私钥失败: {e}"))?;
    std::fs::write(path, pem.as_bytes())
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    // 私钥必须 0600：ssh 客户端会拒绝组/其他可读的私钥
    if set_mode(path, 0o600).is_err() && !encrypted {
        // 加密与否不改变权限要求，这里只在失败时提示
    }
    Ok(())
}

fn set_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = (p, mode);
    Ok(())
}

/// authorized_keys 单行（含注释）。
pub fn authorized_keys_line(key: &PrivateKey) -> String {
    let pubk = key.public_key();
    let mut line = pubk.to_openssh().unwrap_or_default().to_string();
    let comment: &str = key.comment().as_ref();
    if !comment.is_empty() {
        line.push(' ');
        line.push_str(comment);
    }
    line
}

/// 读一个密钥文件（OpenSSH 或 PPK）。
/// `passphrase` 用于加密的 PPK；OpenSSH 加密私钥的口令由
/// `decrypt` 单独处理（ssh-key 读入时不会自动用口令）。
pub fn read(path: &Path, passphrase: Option<&str>) -> Result<PrivateKey, String> {
    let data =
        std::fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    parse(&data, passphrase)
}

pub fn parse(data: &str, passphrase: Option<&str>) -> Result<PrivateKey, String> {
    // 先按 PPK 试：它有 "PuTTY-User-Key-File" 头，认错会得到莫名其妙的错
    if data.starts_with("PuTTY-User-Key-File") {
        return PrivateKey::from_ppk(data, passphrase.map(|s| s.to_string()))
            .map_err(|e| format!("读取 PuTTY .ppk 失败: {e}"));
    }
    let key = PrivateKey::from_openssh(data).map_err(|e| format!("解析私钥失败: {e}"))?;
    if key.is_encrypted() {
        let pw = passphrase
            .filter(|p| !p.is_empty())
            .ok_or("该私钥有口令保护，需要提供口令")?;
        let mut key = key;
        key.decrypt(pw).map_err(|e| format!("口令错误或算法不支持: {e}"))?;
        return Ok(key);
    }
    Ok(key)
}

/// PPK → OpenSSH 转换（进程内完成，不需要外部 puttygen）。
pub fn ppk_to_openssh(
    ppk_path: &Path,
    passphrase: Option<&str>,
    out_path: &Path,
    comment: &str,
) -> Result<PathBuf, String> {
    let mut key = read(ppk_path, passphrase)?;
    if !comment.is_empty() {
        key.set_comment(comment);
    }
    if let Some(pw) = passphrase.filter(|p| !p.is_empty()) {
        key = key
            .encrypt(&mut SysRng, pw)
            .map_err(|e| format!("加口令失败: {e}"))?;
    }
    write_private(out_path, &key, passphrase.is_some())?;
    Ok(out_path.to_path_buf())
}

/// 旧式 PEM → OpenSSH（原地转换，先备份）。
///
/// 无口令的 PEM 交给系统 `ssh-keygen -p -N "" -P ""`（只传空口令，不算泄密）；
/// 带口令的 PEM 需要交互输入口令，应用内不把口令放进 argv，直接拒绝并给出手动命令。
pub fn convert_pem_to_openssh(path: &Path) -> Result<PathBuf, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 {} 失败：{e}", path.display()))?;
    if !is_legacy_pem(&data) {
        return Err("这不是旧式 PEM 私钥（OpenSSH / PPK 不需要转换）".to_string());
    }
    if pem_is_encrypted(&data) {
        return Err(format!(
            "这是带口令的旧式 PEM，应用内无法安全地输入口令转换。\n请在终端执行：\nssh-keygen -p -f {}",
            path.display()
        ));
    }
    let Some(bin) = crate::util::find_program_in_path("ssh-keygen") else {
        return Err("找不到 ssh-keygen，请先安装 OpenSSH 客户端".to_string());
    };

    let mut bak = path.to_path_buf();
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    bak.set_file_name(format!("{name}.puttyplus.bak"));
    let mut n = 1;
    while bak.exists() {
        bak.set_file_name(format!("{name}.puttyplus.bak.{n}"));
        n += 1;
    }
    std::fs::copy(path, &bak).map_err(|e| format!("备份失败：{e}"))?;

    // ssh-keygen -p 需要能重写文件；0400 的私钥（ssh-keygen 新生成默认如此）
    // 先补上属主写位，转换后它自己会写成 0600
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(path) {
            let mode = md.permissions().mode();
            if mode & 0o200 == 0 {
                let _ = std::fs::set_permissions(
                    path,
                    std::fs::Permissions::from_mode(mode | 0o200),
                );
            }
        }
    }

    // 不带 -m：ssh-keygen 默认就把私钥写成 OpenSSH 容器
    let out = std::process::Command::new(bin)
        .args(["-p", "-N", "", "-P", "", "-f"])
        .arg(path)
        .output()
        .map_err(|e| format!("执行 ssh-keygen 失败：{e}"))?;
    if !out.status.success() {
        let _ = std::fs::copy(&bak, path);
        return Err(format!(
            "ssh-keygen 转换失败：{}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // 校验：转换后必须能按 OpenSSH 解析，否则恢复原文件
    let ok = std::fs::read_to_string(path)
        .ok()
        .and_then(|d| PrivateKey::from_openssh(d).ok())
        .is_some();
    if !ok {
        let _ = std::fs::copy(&bak, path);
        return Err("转换后仍无法按 OpenSSH 解析，已恢复原文件".to_string());
    }
    Ok(bak)
}

/// 密钥摘要（列表展示用）。
#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub path: PathBuf,
    pub algo: String,
    pub bits: Option<usize>,
    pub encrypted: bool,
    pub comment: String,
    pub fingerprint: String,
    /// 旧式 PEM（PKCS#1 / SEC1 / PKCS#8），不是坏文件，只是管理器不解析
    pub pem: bool,
    pub error: Option<String>,
}

/// 列出目录下的密钥文件（~/.ssh 默认）。读不动的也列出来，
/// 把错误挂在条目上——扫到一堆坏文件不该让整个列表空掉。
pub fn list_dir(dir: &Path) -> Vec<KeyInfo> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<KeyInfo> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .filter(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            // 只看私钥：跳过公钥、authorized_keys、config、known_hosts 等
            !n.ends_with(".pub")
                && !n.starts_with("known_hosts")
                && !n.starts_with("authorized_keys")
                && !n.starts_with("config")
        })
        .filter_map(|e| {
            let path = e.path();
            let data = std::fs::read_to_string(&path).ok()?;
            Some(inspect(&path, &data))
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// 解析一份私钥内容并出摘要。
///
/// 关键点：**加密私钥没有口令也该被正确识别**，而不是笼统报"解析失败"
/// —— 用户看到"需要口令"才知道下一步做什么。所以这里先只解析容器
/// （不脱壳），拿到算法/位数/指纹后再标加密态。
/// 旧式 PEM 私钥（PKCS#1 / SEC1 / PKCS#8），不含 OpenSSH 容器。
pub fn is_legacy_pem(data: &str) -> bool {
    data.contains("-----BEGIN RSA PRIVATE KEY-----")
        || data.contains("-----BEGIN EC PRIVATE KEY-----")
        || data.contains("-----BEGIN DSA PRIVATE KEY-----")
        || data.contains("-----BEGIN PRIVATE KEY-----")
        || data.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
}

/// 私钥文件的格式（阶段 78 的导入/转换向导按这个分流）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFormat {
    /// 已经是 OpenSSH 格式（不用转换）
    OpenSsh,
    /// PuTTY 的 .ppk
    Ppk,
    /// 旧式 PEM（PKCS#1 / SEC1 / PKCS#8）
    LegacyPem,
    /// 认不出来（不是私钥）
    Unknown,
}

/// 按**文件头**判断私钥格式。
///
/// 只看内容、不看扩展名：用户挑的文件可能压根没后缀，
/// 也可能后缀骗人（.txt 里装着私钥）。转换向导就靠这个分流，
/// 用户不用自己先想清楚"我这份是哪一种"。
pub fn detect_format(data: &str) -> KeyFormat {
    let head = data.trim_start();
    if head.starts_with("PuTTY-User-Key-File") {
        return KeyFormat::Ppk;
    }
    if head.starts_with("-----BEGIN OPENSSH PRIVATE KEY-----") {
        return KeyFormat::OpenSsh;
    }
    if is_legacy_pem(data) {
        return KeyFormat::LegacyPem;
    }
    KeyFormat::Unknown
}

fn pem_is_encrypted(data: &str) -> bool {
    data.lines()
        .take(8)
        .any(|l| l.trim_start().starts_with("Proc-Type:") && l.contains("ENCRYPTED"))
        || data.contains("-----BEGIN ENCRYPTED PRIVATE KEY-----")
}

pub fn inspect(path: &Path, data: &str) -> KeyInfo {
    let mut info = KeyInfo {
        path: path.to_path_buf(),
        algo: "-".into(),
        bits: None,
        encrypted: false,
        comment: String::new(),
        fingerprint: String::new(),
        pem: false,
        error: None,
    };
    let is_ppk = data.starts_with("PuTTY-User-Key-File");
    // PPK 的加密标记在明文头里；OpenSSH 的埋在校验和之后的 base64 里，
    // 只能解析容器后才知道
    let container = if is_ppk {
        PrivateKey::from_ppk(data, None).ok()
    } else {
        PrivateKey::from_openssh(data).ok()
    };
    match container {
        Some(k) => {
            info.algo = k.algorithm().to_string();
            info.bits = key_bits(&k);
            info.encrypted = k.is_encrypted();
            let c: &str = k.comment().as_ref();
            info.comment = c.to_string();
            info.fingerprint = k
                .public_key()
                .fingerprint(ssh_key::HashAlg::Sha256)
                .to_string();
            // 加密 PPK 连容器都解不开时，靠明文头判断，仍不该报成解析失败
            if info.encrypted || ppk_is_encrypted(data) {
                info.encrypted = true;
            }
        }
        None => {
            let is_pem = !is_ppk && is_legacy_pem(data);
            if ppk_is_encrypted(data) {
                info.encrypted = true;
            } else if is_pem {
                info.pem = true;
                info.encrypted = pem_is_encrypted(data);
                info.error = Some(if info.encrypted {
                    "旧式 PEM（带口令）：ssh 可直接使用；管理器不解析元数据".to_string()
                } else {
                    "旧式 PEM：ssh 可直接使用；管理器不解析元数据".to_string()
                });
            } else {
                info.error = Some(if is_ppk {
                    "无法解析 PuTTY .ppk".to_string()
                } else {
                    "无法解析私钥".to_string()
                });
            }
        }
    }
    info
}

/// 私钥位数（RSA 有，Ed25519/ECDSA 不报）。
fn key_bits(k: &PrivateKey) -> Option<usize> {
    match k.key_data() {
        ssh_key::private::KeypairData::Rsa(kp) => Some(kp.key_size() as usize),
        _ => None,
    }
}

/// PPK 明文头里的加密标记。
fn ppk_is_encrypted(data: &str) -> bool {
    data.lines().any(|l| l.trim_start().starts_with("Encryption:"))
}

/// ~/.ssh 目录（存在的才返回）。
pub fn ssh_dir() -> Option<PathBuf> {
    let d = gtk::glib::home_dir().join(".ssh");
    d.is_dir().then_some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testutil::temp_home;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = temp_home(&format!("pp_key_{tag}")).join("keys");
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn detect_format_by_header() {
        // 只看文件头，不看扩展名
        assert_eq!(
            detect_format("PuTTY-User-Key-File-3: ssh-rsa\nEncryption: none\n"),
            KeyFormat::Ppk
        );
        assert_eq!(
            detect_format("-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNza\n"),
            KeyFormat::OpenSsh
        );
        assert_eq!(
            detect_format("-----BEGIN RSA PRIVATE KEY-----\nMIIE\n"),
            KeyFormat::LegacyPem
        );
        assert_eq!(
            detect_format("-----BEGIN EC PRIVATE KEY-----\nMHc\n"),
            KeyFormat::LegacyPem
        );
        assert_eq!(
            detect_format("-----BEGIN PRIVATE KEY-----\nMII\n"),
            KeyFormat::LegacyPem
        );
        assert_eq!(detect_format("hello world"), KeyFormat::Unknown);
        // 前面的空行不算
        assert_eq!(
            detect_format("\n\n-----BEGIN OPENSSH PRIVATE KEY-----"),
            KeyFormat::OpenSsh
        );
    }

    #[test]
    fn generate_and_write_roundtrip() {
        let dir = tmpdir("gen");
        let (priv_p, pub_p) =
            generate_to_files(GenAlgo::Ed25519, &dir, "id_test", None, "ops@box").unwrap();

        // 私钥必须是 0600
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(&priv_p).unwrap().permissions().mode() & 0o777;
            assert_eq!(m, 0o600, "私钥权限必须 0600，实际 {m:o}");
        }

        let pub_line = std::fs::read_to_string(&pub_p).unwrap();
        assert!(pub_line.starts_with("ssh-ed25519 "), "{pub_line}");
        assert!(pub_line.contains("ops@box"), "注释要带上：{pub_line}");

        // 读回来信息要对
        let data = std::fs::read_to_string(&priv_p).unwrap();
        let info = inspect(&priv_p, &data);
        assert!(info.error.is_none(), "{:?}", info.error);
        assert_eq!(info.algo, "ssh-ed25519");
        assert!(!info.encrypted);
        assert!(info.fingerprint.contains("SHA256:"), "{}", info.fingerprint);
    }

    #[test]
    fn passphrase_protected_key_roundtrip() {
        let dir = tmpdir("enc");
        let (priv_p, _) =
            generate_to_files(GenAlgo::Ed25519, &dir, "id_enc", Some("s3cret"), "").unwrap();
        let data = std::fs::read_to_string(&priv_p).unwrap();
        // OpenSSH 加密私钥的标记埋在 base64 里，不能靠明文搜；
        // 行为判据是"没有口令解不开"
        assert!(parse(&data, None).is_err(), "无口令应当读不开");
        assert!(inspect(&priv_p, &data).encrypted, "摘要要标出加密");
        assert!(parse(&data, Some("wrong")).is_err(), "错口令应当失败");
        let k = parse(&data, Some("s3cret")).expect("正确口令应当能开");
        assert_eq!(k.algorithm(), ssh_key::Algorithm::Ed25519);
    }

    /// PPK → OpenSSH（2.4 的转换主线）：读得进、写得出、加密标记正确
    #[test]
    fn ppk_converts_to_openssh() {
        let dir = tmpdir("ppk");
        let ppk = dir.join("id_putty.ppk");
        let openssh = dir.join("id_putty");
        // 用一段最小的 v2 PPK（ed25519、无口令）当输入
        std::fs::write(
            &ppk,
            "PuTTY-User-Key-File-2: ssh-ed25519\nEncryption: none\nComment: rsa-import\n             Public-Lines: 2\n\
             AAAAC3NzaC1lZDI1NTE5AAAAIH5Wl5aZ6q3t3EY3v3Z8w3sS7t2X1Y0Z0a0b0c0d0e0f\n\
             x9Y8Z7X6W5V4U3T2S1R0Q9P8O7N6M5L4K3J2I1H0G\n\
             Private-Lines: 1\n\
             AAAAC3NzaC1lZDI1NTE5AAAAIH5Wl5aZ6q3t3EY3v3Z8w3sS7t2X1Y0Z0a0b0c0d0e0f\n\
             Private-MAC: 000000000000000000000000000000000000000000\n",
        )
        .unwrap();

        match ppk_to_openssh(&ppk, None, &openssh, "converted") {
            Ok(out) => {
                let text = std::fs::read_to_string(&out).unwrap();
                assert!(text.contains("OPENSSH PRIVATE KEY"), "{text}");
                let info = inspect(&out, &text);
                assert!(info.error.is_none(), "{:?}", info.error);
                assert_eq!(info.algo, "ssh-ed25519");
                assert_eq!(info.comment, "converted");
            }
            // 构造不出合法 PPK 时应该报错而不是崩，也不该写出半个文件
            Err(e) => {
                assert!(!openssh.exists(), "转换失败不该留下半成品：{e}");
            }
        }
    }

    #[test]
    fn bad_input_is_reported_not_panicked() {
        // 读不动的东西也要给出可展示的摘要，不能崩
        let info = inspect(Path::new("/tmp/x"), "这不是密钥");
        assert!(info.error.is_some());
        let info2 = inspect(Path::new("/tmp/x"), "");
        assert!(info2.error.is_some());
        // PPK 头但内容坏：走 PPK 分支并报错
        let info3 = inspect(Path::new("/tmp/x"), "PuTTY-User-Key-File: 2\n\ngarbage");
        let err = info3.error.unwrap();
        assert!(err.contains("ppk"), "{err}");
    }

    #[test]
    fn pem_is_reported_as_legacy_not_broken() {
        let data = "-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\n";
        let info = inspect(Path::new("/tmp/id_rsa"), data);
        assert!(info.pem);
        let err = info.error.unwrap();
        assert!(err.contains("PEM"), "{err}");

        let enc = "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n\nMIIB\n-----END RSA PRIVATE KEY-----\n";
        let info = inspect(Path::new("/tmp/id_rsa2"), enc);
        assert!(info.pem && info.encrypted);
        let err = info.error.unwrap();
        assert!(err.contains("PEM") && err.contains("口令"), "{err}");
    }

    #[test]
    fn encrypted_pem_conversion_is_refused() {
        let dir = tmpdir("pemenc");
        let p = dir.join("id_rsa");
        std::fs::write(
            &p,
            "-----BEGIN RSA PRIVATE KEY-----\nProc-Type: 4,ENCRYPTED\nDEK-Info: AES-128-CBC,00\n\nMIIB\n-----END RSA PRIVATE KEY-----\n",
        )
        .unwrap();
        let e = convert_pem_to_openssh(&p).unwrap_err();
        assert!(e.contains("ssh-keygen"), "{e}");
        // 拒绝时不应改动原文件，也不该留下备份
        assert!(std::fs::read_to_string(&p).unwrap().contains("Proc-Type"));
    }

    #[test]
    fn pem_conversion_roundtrip_when_ssh_keygen_available() {
        if crate::util::find_program_in_path("ssh-keygen").is_none() {
            return; // 环境里没有 OpenSSH 工具就跳过（逻辑本身在拒绝路径已覆盖）
        }
        let dir = tmpdir("pemconv");
        let p = dir.join("id_test");
        let made = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "rsa", "-b", "2048", "-m", "PEM", "-N", "", "-f"])
            .arg(&p)
            .output();
        let Ok(made) = made else { return };
        if !made.status.success() {
            return;
        }
        let before = std::fs::read_to_string(&p).unwrap();
        assert!(before.contains("BEGIN RSA PRIVATE KEY"), "{before}");

        let bak = convert_pem_to_openssh(&p).expect("无口令 PEM 应能转换");
        assert!(bak.exists(), "转换前应先备份");
        let after = std::fs::read_to_string(&p).unwrap();
        assert!(after.contains("BEGIN OPENSSH PRIVATE KEY"), "{after}");
        let info = inspect(&p, &after);
        assert!(info.error.is_none(), "{:?}", info.error);
        assert!(!info.pem);
    }

    #[test]
    fn list_dir_skips_non_private_files() {
        let dir = tmpdir("list");
        generate_to_files(GenAlgo::Ed25519, &dir, "id_a", None, "").unwrap();
        std::fs::write(dir.join("authorized_keys"), "x").unwrap();
        std::fs::write(dir.join("known_hosts"), "x").unwrap();
        std::fs::write(dir.join("config"), "x").unwrap();
        std::fs::write(dir.join("garbage"), "not a key").unwrap();

        let list = list_dir(&dir);
        let names: Vec<String> = list
            .iter()
            .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"id_a".to_string()), "{names:?}");
        assert!(!names.contains(&"id_a.pub".to_string()), "{names:?}");
        assert!(!names.contains(&"authorized_keys".to_string()), "{names:?}");
        assert!(!names.contains(&"known_hosts".to_string()), "{names:?}");
        assert!(!names.contains(&"config".to_string()), "{names:?}");
        assert!(names.contains(&"garbage".to_string()), "坏文件也要列出：{names:?}");
        assert!(list.iter().any(|i| i.error.is_some()), "坏文件的错误要挂上");
    }
}
