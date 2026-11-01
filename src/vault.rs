//! 凭据库（2.1）：主密码 + Argon2id 派生密钥 + AES-256-GCM 加密落盘。
//!
//! 存 `~/.config/puttyplus/credentials.vault`（独立文件，不混进
//! connections.ini——免得有人 grep 一下 ini 就以为没密码了）。
//!
//! ```text
//! [vault]
//! version=2
//! protected=true|false
//! kdf=argon2id,m=65536,t=3,p=1     # protected=true 时才有
//! salt=<base64 16B>                # protected=true 时才有
//! data=<base64 nonce(12B) || ciphertext+tag>   # protected=true
//! data=<base64 明文负载>            # protected=false
//! ```
//!
//! **protected=false = 空主密码模式**（堡垒机场景）：不加密、不用主密码、
//! 打开就用。⚠️ 这时文件里的密码是**明文**——所以必须在文件头**明写**
//! `protected=false`，让人一看就知道这份凭据库**没有**加密保护，而不是
//! 看到一堆 base64 就以为它是安全的。
//!
//! （另一种做法是"空密码也走 Argon2id+AES-GCM"——但那等于**假装**加密：
//! 空串派生出的密钥谁都能算出来，文件看起来是密文、实际一戳就破。宁可
//! 老实标出来。）
//!
//! 老库（`version=1`，没有 `protected` 键）一律当 `protected=true`。
//!
//! 负载格式两种模式都一样：`<base64(连接名)> <base64(密码)>` 一行一条，
//! 不引 serde。
//!
//! ## 解锁策略
//!
//! **用到才解锁**，不在启动时拦一道：本项目常被当堡垒机 launcher 用
//! （`spawn_self` 派生新窗口、PUTTYPLUS_PW_SESSION 传密码），每次启动
//! 都敲主密码会没法用。解锁后保持到进程退出。
//!
//! 依赖 argon2id 是因为它抗 GPU 爆破；AES-GCM 带认证标签，密文被改过
//! 会解密失败而不是吐垃圾。主密码与派生密钥全程 `Zeroizing`，内存里
//! 不留副本。

use std::path::PathBuf;
use std::sync::Mutex;

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::Engine;
use zeroize::Zeroizing;

use gtk::glib;

/// Argon2id 参数。m=64MiB / t=3 是 2026 年的保守起点：桌面机 ~0.2s，
/// 离线爆破 8 位密码不现实。真要更高可以改这里，但改了就该重建库
/// （参数进了文件头，老库仍按老参数解）。
const M_COST: u32 = 65536;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;

const B64: base64::engine::general_purpose::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn path() -> PathBuf {
    // 环境变量可覆盖：测试与调试时想用别的库，不必动真实凭据。
    // （测试写用户真实配置是明令禁止的——凭据库尤其不能碰。）
    if let Some(p) = std::env::var_os("PUTTYPLUS_VAULT") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir()
        .join("puttyplus")
        .join("credentials.vault")
}

pub fn exists() -> bool {
    path().is_file()
}

/// 凭据库的状态（给界面上的**状态标志**用）。
///
/// **为什么要有它**：凭据库是"开了才生效"的开关，而**开了没有、密码存到
/// 哪去了，全靠用户记得**——用户忘了自己没开凭据库，就会以为"记住密码"
/// 把密码加密存起来了，其实它明文躺在 `connections.ini` 里。界面上得**一直
/// 看得见**当前是什么状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// **没建库**（凭据库没启用）：勾「记住密码」会明文写进 `connections.ini`。
    Off,
    /// 建了库、有主密码保护、**当前已解锁**。
    Unlocked,
    /// 建了库、有主密码保护、**当前锁着**（要用得输主密码）。
    Locked,
    /// 建了库，但**没有主密码**（明文模式）：密码明文躺在本机文件里。
    Plain,
}

/// 当前状态。
pub fn state() -> State {
    if !exists() {
        return State::Off;
    }
    if !is_protected() {
        return State::Plain;
    }
    if is_unlocked() {
        State::Unlocked
    } else {
        State::Locked
    }
}

/* ---------------- 主密码解锁状态（进程内） ---------------- */

/// 解锁后驻留的派生密钥。进程退出即消失，不落盘、不跨进程复用。
static UNLOCKED: Mutex<Option<Zeroizing<[u8; KEY_LEN]>>> = Mutex::new(None);

/// 解锁时缓存的主密码：写凭据要重新加密（换 nonce 重算密文），
/// 每次都问用户没法用，故缓存并全程 Zeroizing。这是"用主密码加密"的
/// 固有代价——主密码在本进程内必然以某种形式驻留内存。
static MASTER: Mutex<Option<Zeroizing<String>>> = Mutex::new(None);

pub fn is_unlocked() -> bool {
    UNLOCKED.lock().map(|k| k.is_some()).unwrap_or(false)
}

pub fn lock() {
    if let Ok(mut k) = UNLOCKED.lock() {
        k.take(); // Zeroizing 析构时抹零
    }
    if let Ok(mut m) = MASTER.lock() {
        m.take();
    }
}

/// 已解锁时借出派生密钥（调用方负责尽快结束借用）。
fn with_key<T>(f: impl FnOnce(&[u8]) -> T) -> Option<T> {
    let guard = UNLOCKED.lock().ok()?;
    let key = guard.as_ref()?;
    Some(f(key.as_slice()))
}

fn master_pw() -> Option<Zeroizing<String>> {
    MASTER.lock().ok().and_then(|m| m.clone())
}

/* ---------------- 加解密核心 ---------------- */

fn derive_key(master: &str, salt: &[u8; SALT_LEN]) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    derive_with(master, salt, M_COST, T_COST, P_COST)
}

fn derive_with(
    master: &str,
    salt: &[u8; SALT_LEN],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    let params = argon2::Params::new(m_cost, t_cost, p_cost, Some(KEY_LEN))
        .map_err(|e| format!("Argon2 参数无效: {e}"))?;
    let a2 = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        params,
    );
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    a2.hash_password_into(master.as_bytes(), salt, key.as_mut())
        .map_err(|e| format!("密钥派生失败: {e}"))?;
    Ok(key)
}

/// 负载编码：一行一条 `<base64(连接名)> <base64(密码)`。
///
/// 不用 KeyFile —— 它只在接受键名时**静默拒绝**含 `=` 的名字，
/// 而连接名是用户起的，叫 `a=b` 完全可能：密码就这么无声丢了。
/// base64 标准字母表不含空格与换行，按空格切分没有歧义，
/// 也不需要自己写转义。
fn encode_creds(creds: &[(String, Zeroizing<String>)]) -> String {
    let mut out = String::new();
    for (name, pw) in creds {
        out.push_str(&B64.encode(name.as_bytes()));
        out.push(' ');
        out.push_str(&B64.encode(pw.as_bytes()));
        out.push('\n');
    }
    out
}

fn decode_creds(data: &str) -> Vec<(String, Zeroizing<String>)> {
    data.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| {
            let (n, p) = line.split_once(' ')?;
            let name = B64.decode(n).ok()?;
            let pw = B64.decode(p).ok()?;
            Some((
                String::from_utf8(name).ok()?,
                Zeroizing::new(String::from_utf8(pw).ok()?),
            ))
        })
        .collect()
}

/// 建新库（或用同一主密码覆盖重建）：返回是否成功。
/// 已存在的库会被覆盖——调用方必须先确认用户知道自己在做什么
/// （重设主密码就是重设全部凭据）。
pub fn create(master: &str, creds: &[(String, Zeroizing<String>)]) -> Result<(), String> {
    create_at(&path(), master, creds)
}

/// 指定路径建库（**测试用**）。
fn create_at(
    p: &std::path::Path,
    master: &str,
    creds: &[(String, Zeroizing<String>)],
) -> Result<(), String> {
    let kf = glib::KeyFile::new();
    kf.set_integer("vault", "version", 2);
    let payload = encode_creds(creds);
    if master.is_empty() {
        // **空主密码 = 不加密**：明文存，文件头明写 protected=false。
        kf.set_boolean("vault", "protected", false);
        kf.set_string("vault", "data", &B64.encode(payload.as_bytes()));
        return write_kf_at(p, &kf);
    }
    let salt: [u8; SALT_LEN] = random_bytes()?;
    let key = derive_key(master, &salt)?;
    let cipher = new_cipher(key.as_ref());
    let nonce_bytes: [u8; NONCE_LEN] = random_bytes()?;
    let ct = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            payload.as_bytes(),
        )
        .map_err(|_| "加密失败".to_string())?;
    let mut blob = Vec::with_capacity(NONCE_LEN + ct.len());
    blob.extend_from_slice(&nonce_bytes);
    blob.extend_from_slice(&ct);

    kf.set_boolean("vault", "protected", true);
    kf.set_string(
        "vault",
        "kdf",
        &format!("argon2id,m={M_COST},t={T_COST},p={P_COST}"),
    );
    kf.set_string("vault", "salt", &B64.encode(salt));
    kf.set_string("vault", "data", &B64.encode(&blob));
    write_kf_at(p, &kf)
}

/// 这个库**有没有**主密码保护（文件头说了算）。
///
/// `protected=false` = 空主密码模式：密码是明文存的，打开就能用。
/// 老库（没有 `protected` 键）一律按 `true` 处理。
pub fn is_protected() -> bool {
    read_kf().map(|kf| kf_protected(&kf)).unwrap_or(true)
}

/// 指定路径的库有没有保护（**测试用**）。
fn is_protected_at(p: &std::path::Path) -> bool {
    read_kf_at(p).map(|kf| kf_protected(&kf)).unwrap_or(true)
}

/// 文件头里的 `protected` 标志。**缺键 = 有保护**（老库兼容）。
fn kf_protected(kf: &glib::KeyFile) -> bool {
    kf.boolean("vault", "protected").unwrap_or(true)
}

/// 用主密码解锁；密码错时返回 Err（认证标签校验失败）。
pub fn unlock(master: &str) -> Result<(), String> {
    unlock_at(&path(), master)
}

/// 指定路径解锁（**测试用**）。
fn unlock_at(p: &std::path::Path, master: &str) -> Result<(), String> {
    // 空主密码模式：没有密码要验，直接算解锁成功。
    //
    // ⚠️ **主密码一律记空串**（不是用户随手敲的那个）：明文库的主密码
    // 就是"没有主密码"。要是把用户敲的字符串记进去，`save_all` 下次写凭据
    // 就会用那个密码**把明文库悄悄变成加密库**——用户既没勾"设主密码"，
    // 也再解不开了。
    if !is_protected_at(p) {
        decrypt_with_at(p, "")?;
        set_master("");
        if let Ok(mut k) = UNLOCKED.lock() {
            *k = Some(Zeroizing::new([0u8; KEY_LEN]));
        }
        return Ok(());
    }
    // 先真解一次：主密码错 / 文件被篡改要在写入内存状态**之前**发现
    decrypt_with_at(p, master)?;
    let key = reload_key_at(p, master)?;
    if let Ok(mut k) = UNLOCKED.lock() {
        *k = Some(key);
    }
    Ok(())
}

/// 记下当前用的主密码（空主密码模式记空串）。
fn set_master(master: &str) {
    if let Ok(mut m) = MASTER.lock() {
        *m = Some(Zeroizing::new(master.to_string()));
    }
}

/// **重置凭据库**：丢掉旧库，用新主密码建一个**空**库。
///
/// **为什么非要有这个手段**：主密码是**唯一**的解密钥匙。忘了它（或者文件
/// 损坏）就再也解不开——Argon2id + AES-GCM 没有后门，这是密码学的必然，
/// 不是偷懒。但"解不开"不能等于"从此再也存不了密码"：用户能接受的是
/// "密码重新输一遍"，不能接受的是"永远没法用凭据库"。所以必须给一条
/// **明确可行、后果说得清**的出路。
///
/// **后果（UI 里必须说清）**：旧库里保存的密码**永久丢失**——不是"暂时
/// 取不出来"，是那串密文再也解不开。
///
/// **不受影响的**（这才是用户敢点的原因）：
/// - 连接列表（`connections.ini`）；
/// - 数据库连接的定义（`databases.ini`：地址/端口/用户/库名）；
/// - 数据库连接里的「记住密码」勾选。
///
/// 所以重置后重新连上某个库，弹一次密码框、勾一下「记住密码」，就又存进
/// 新库了——用户要做的只是**重新输入**，不是"重建一切"。
///
/// **安全**：先清掉内存里的解锁状态，否则会出现"新库配旧密钥"的错配。
pub fn reset(new_master: &str) -> Result<(), String> {
    reset_at(&path(), new_master)
}

/// 指定路径重置（**测试用**）。
fn reset_at(p: &std::path::Path, new_master: &str) -> Result<(), String> {
    // ⚠️ 空主密码**允许**（= 不加密模式）：重置本来就是"重新开始"，
    // 堡垒机场景下用户明确要"不设主密码"是合理诉求。
    // （改主密码那条路**不许**降级，见 `change_master_at`。）
    // 旧库已经没用了（主密码忘了 / 文件坏了），但**内存里的解锁状态**
    // 得先清掉：否则"新主密码的库"会挂着"旧主密码的派生密钥"。
    lock();
    // 新 salt + 新 nonce + 空凭据 = 一个全新的空库。
    // `write_kf` 是**原子替换**（先写临时文件再 rename），所以要么旧库
    // 还在、要么新库就位，不会出现"写了一半的半截库"。
    create_at(p, new_master, &[])
}

/// **改主密码**：用旧密码解开，用新密码**重新加密**（凭据原样带过去）。
///
/// **为什么要单独一条路**：用户设的主密码可能太随手（`123456`），或者
/// 换机器时想统一成一个。没有这条路就只能"重置"——而重置会把凭据**全丢
/// 了**，为了改个密码代价太大。
///
/// - **旧密码错就拒绝**：这一步就是"你得知道旧密码"的证明。
/// - **明文库（空主密码）升级成加密库**：`old` 传空串即可——明文库本来
///   就没有密码要验，凭据原样搬进加密库。
/// - ⚠️ **加密库降级成明文库：不许**（`new` 传空串 → 拒绝）。那是**安全
///   倒退**，不该跟"改密码"混在一个动作里；要降级请走「重置凭据库」——
///   那里有"会永久丢失什么"的后果确认。
///
/// 凭据**一条不少**地搬过去——这是它跟 `reset` 的根本区别。
pub fn change_master(old: &str, new: &str) -> Result<(), String> {
    change_master_at(&path(), old, new)
}

/// 指定路径改主密码（**测试用**）。
fn change_master_at(p: &std::path::Path, old: &str, new: &str) -> Result<(), String> {
    // ⚠️ **不许降级**成不加密：留空 = 把一份加密库改成明文，那是安全
    // 倒退，不该跟"改密码"混在一个动作里（要降级请重置——那里有后果确认）。
    if new.is_empty() {
        return Err("新主密码不能为空（改成不加密请用「重置凭据库」）".into());
    }
    // 先用旧密码解一次（**不依赖全局解锁状态**，所以解锁状态是旧的还是
    // 新的都不影响）；旧密码错 → 拒绝，文件一个字都没动。
    let creds = decrypt_with_at(p, old)?;
    // 用新密码重建整个库（原子写：要么旧库还在、要么新库就位）
    create_at(p, new, &creds)?;
    // 文件已经是新密码了 → 内存里的旧密钥必须换掉
    lock();
    unlock_at(p, new)
}

/// 已解锁时列出全部凭据的连接名（不取密码，避免无谓地把秘密搬出来）。
pub fn names() -> Vec<String> {
    read_all()
        .unwrap_or_default()
        .into_iter()
        .map(|(n, _)| n)
        .collect()
}

/// 取一条凭据（未解锁 → None）。
pub fn get(name: &str) -> Option<Zeroizing<String>> {
    let all = read_all()?;
    all.into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, p)| p)
}

/// 写一条凭据（未解锁 → false）。
pub fn put(name: &str, password: &str) -> bool {
    let mut all = match read_all() {
        Some(v) => v,
        None => return false,
    };
    match all.iter_mut().find(|(n, _)| n == name) {
        Some(slot) => slot.1 = Zeroizing::new(password.to_string()),
        None => all.push((name.to_string(), Zeroizing::new(password.to_string()))),
    }
    save_all(&all)
}

/// 删一条凭据。
pub fn remove(name: &str) -> bool {
    let Some(mut all) = read_all() else {
        return false;
    };
    let before = all.len();
    all.retain(|(n, _)| n != name);
    if all.len() == before {
        return false;
    }
    save_all(&all)
}

fn reload_key(master: &str) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    reload_key_at(&path(), master)
}

fn reload_key_at(p: &std::path::Path, master: &str) -> Result<Zeroizing<[u8; KEY_LEN]>, String> {
    let kf = read_kf_at(p)?;
    let salt = decode_salt(&kf)?;
    let key = derive_key(master, &salt)?;
    if let Ok(mut m) = MASTER.lock() {
        *m = Some(Zeroizing::new(master.to_string()));
    }
    Ok(key)
}

/// 解密全部凭据（需要主密码）。
fn decrypt_with(master: &str) -> Result<Vec<(String, Zeroizing<String>)>, String> {
    decrypt_with_at(&path(), master)
}

fn decrypt_with_at(
    p: &std::path::Path,
    master: &str,
) -> Result<Vec<(String, Zeroizing<String>)>, String> {
    let kf = read_kf_at(p)?;
    // 明文模式：没有密钥，直接读负载（`master` 用不上）。
    if !kf_protected(&kf) {
        let blob = B64
            .decode(kf.string("vault", "data").map_err(|_| "凭据库缺少 data 字段")?)
            .map_err(|e| format!("data 字段不是合法 base64: {e}"))?;
        return Ok(decode_creds(&String::from_utf8_lossy(&blob)));
    }
    let salt = decode_salt(&kf)?;
    let blob = B64
        .decode(kf.string("vault", "data").map_err(|_| "凭据库缺少 data 字段")?)
        .map_err(|e| format!("data 字段不是合法 base64: {e}"))?;
    if blob.len() < NONCE_LEN {
        return Err("凭据库数据段长度异常".into());
    }
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let key = derive_key(master, &salt)?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key.as_ref()));
    let pt = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            ct,
        )
        .map_err(|_| "主密码错误，或凭据库文件已被篡改".to_string())?;
    Ok(decode_creds(&String::from_utf8_lossy(&pt)))
}

/// 已解锁状态下读全部凭据。
fn read_all() -> Option<Vec<(String, Zeroizing<String>)>> {
    let kf = read_kf().ok()?;
    // 明文模式：不需要密钥（但仍然要求"已解锁"——UI 上得先过一道）。
    if !kf_protected(&kf) {
        if !is_unlocked() {
            return None;
        }
        let blob = B64.decode(kf.string("vault", "data").ok()?).ok()?;
        return Some(decode_creds(&String::from_utf8_lossy(&blob)));
    }
    with_key(|key| {
        let blob = B64.decode(kf.string("vault", "data").ok()?).ok()?;
        if blob.len() < NONCE_LEN {
            return None;
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let cipher = new_cipher(key);
        let pt = cipher.decrypt(Nonce::from_slice(nonce), ct).ok()?;
        Some(decode_creds(&String::from_utf8_lossy(&pt)))
    })?
}

fn save_all(creds: &[(String, Zeroizing<String>)]) -> bool {
    // ⚠️ 主密码是**空串**（明文模式）也是一种"有"：`master_pw()` 返回
    // `Some("")`，不是 `None`。`None` 只表示"没解锁"。
    let Some(master) = master_pw() else {
        return false;
    };
    create(&master, creds).is_ok()
}

/// 长度不对会 panic —— 调用方只有两个来源：派生出的 32 字节密钥，
/// 和 static 里类型已保证的 32 字节，不存在第三种。
#[allow(deprecated)] // from_slice：aes-gcm 0.11 的 TryFrom 泛型推导很别扭
fn new_cipher(key: &[u8]) -> Aes256Gcm {
    Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
}

fn decode_salt(kf: &glib::KeyFile) -> Result<[u8; SALT_LEN], String> {
    let b = B64
        .decode(kf.string("vault", "salt").map_err(|_| "凭据库缺少 salt 字段")?)
        .map_err(|e| format!("salt 不是合法 base64: {e}"))?;
    b.try_into().map_err(|_| "salt 长度异常".to_string())
}

fn read_kf() -> Result<glib::KeyFile, String> {
    read_kf_at(&path())
}

/// 指定路径读库（**测试用**：测试不许碰用户真实的凭据库文件）。
fn read_kf_at(p: &std::path::Path) -> Result<glib::KeyFile, String> {
    let kf = glib::KeyFile::new();
    kf.load_from_file(p, glib::KeyFileFlags::NONE)
        .map_err(|e| format!("读取凭据库失败: {e}"))?;
    Ok(kf)
}

fn write_kf(kf: &glib::KeyFile) -> Result<(), String> {
    write_kf_at(&path(), kf)
}

/// 指定路径写库（**原子替换**：先写临时文件再 rename 覆盖）。
fn write_kf_at(p: &std::path::Path, kf: &glib::KeyFile) -> Result<(), String> {
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("创建配置目录失败: {e}"))?;
    }
    let data = kf.to_data();
    // 临时文件名带 pid：程序允许多开（Application 是 NON_UNIQUE），
    // 两个窗口同时写凭据库时不能共用一个临时文件。
    let tmp = p.with_file_name(format!(
        "{}.tmp.{}",
        p.file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("credentials.vault"),
        std::process::id()
    ));
    // 建文件时就是 0600：临时文件存在的那一瞬间也不该是别人能读的
    // （配置目录本身可能是 0755）。
    write_private(&tmp, data.as_bytes())?;
    // **原子替换**：rename 覆盖。老实现是 `fs::write` 直接截断重写——
    // 写到一半被打断（磁盘满、进程被杀）就只剩半截文件，**整个凭据库
    // 连同里面全部密码一起没了**。凭据库是本机唯一一份密码副本，丢不起。
    if let Err(e) = std::fs::rename(&tmp, p) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("替换凭据库失败: {e}"));
    }
    set_private(&p);
    Ok(())
}

/// 写文件并保证只有本人可读（unix 下建文件时就是 0600，不留窗口）。
fn write_private(p: &std::path::Path, data: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(p)
            .map_err(|e| format!("写入 {} 失败: {e}", p.display()))?;
        f.write_all(data)
            .map_err(|e| format!("写入 {} 失败: {e}", p.display()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(p, data).map_err(|e| format!("写入 {} 失败: {e}", p.display()))?;
    }
    set_private(p);
    Ok(())
}

/// 凭据库与 connections.ini 同规格：只有本人可读。
fn set_private(p: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = p;
}

/// 随机字节：优先 getrandom（内核 CSPRNG），失败则报错——
/// 绝不退回弱随机，salt/nonce 弱了等于密钥弱了。
fn random_bytes<const N: usize>() -> Result<[u8; N], String> {
    let mut buf = [0u8; N];
    getrandom::fill(&mut buf).map_err(|e| format!("读取系统随机数失败: {e}"))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注意：本模块的读写走真实用户配置目录（glib 的 user_config_dir
    /// 在进程内缓存，换 HOME 不生效），所以下面只在**纯函数**层面测，
    /// 加解密与派生不碰磁盘。文件层留给真机验证。
    /// 小参数跑语义（快）；生产参数另有一个测试兜底。
    fn cheap(master: &str, salt: &[u8; SALT_LEN]) -> Zeroizing<[u8; KEY_LEN]> {
        derive_with(master, salt, 64, 1, 1).unwrap()
    }

    #[test]
    fn key_derivation_is_deterministic_and_salt_sensitive() {
        let salt_a = [1u8; SALT_LEN];
        let salt_b = [2u8; SALT_LEN];
        let k1 = cheap("correct horse", &salt_a);
        let k2 = cheap("correct horse", &salt_a);
        let k3 = cheap("correct horse", &salt_b);
        let k4 = cheap("wrong horse", &salt_a);
        assert_eq!(k1.as_ref(), k2.as_ref(), "同密码同 salt 必须同密钥");
        assert_ne!(k1.as_ref(), k3.as_ref(), "salt 必须影响密钥");
        assert_ne!(k1.as_ref(), k4.as_ref(), "密码必须影响密钥");
    }

    /// 生产参数必须真的能跑通（Params::new 拒绝的参数会在这里炸，
    /// 而不是在用户第一次解锁时）。
    #[test]
    fn production_params_are_valid() {
        let salt = [7u8; SALT_LEN];
        let k = derive_key("pw", &salt).unwrap();
        assert_eq!(k.as_ref().len(), KEY_LEN);
    }

    /// 加解密全链路（不落盘：直接走内存里的 blob）
    fn seal(master: &str, creds: &[(String, Zeroizing<String>)]) -> (Vec<u8>, [u8; SALT_LEN]) {
        let salt = [3u8; SALT_LEN];
        let key = derive_with(master, &salt, 64, 1, 1).unwrap();
        let cipher = new_cipher(key.as_ref());
        let nonce = [9u8; NONCE_LEN];
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce), encode_creds(creds).as_bytes())
            .unwrap();
        let mut blob = nonce.to_vec();
        blob.extend_from_slice(&ct);
        (blob, salt)
    }

    fn try_unseal(
        master: &str,
        blob: &[u8],
        salt: &[u8; SALT_LEN],
    ) -> Result<Vec<(String, Zeroizing<String>)>, ()> {
        let key = derive_with(master, salt, 64, 1, 1).map_err(|_| ())?;
        let cipher = new_cipher(key.as_ref());
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let pt = cipher.decrypt(Nonce::from_slice(nonce), ct).map_err(|_| ())?;
        Ok(decode_creds(&String::from_utf8_lossy(&pt)))
    }

    fn unseal(master: &str, blob: &[u8], salt: &[u8; SALT_LEN]) -> Vec<(String, Zeroizing<String>)> {
        try_unseal(master, blob, salt).expect("应当能解开")
    }

    #[test]
    fn seal_open_roundtrip() {
        let creds = vec![
            ("堡垒机".into(), Zeroizing::new("pw-1".into())),
            ("a=b".into(), Zeroizing::new("含 = 与\n换行".into())),
        ];
        let (blob, salt) = seal("master", &creds);
        assert_eq!(unseal("master", &blob, &salt), creds);
    }

    /// 主密码错 / 密文被改：必须失败，而不是吐出垃圾明文
    #[test]
    fn wrong_master_and_tampering_are_rejected() {
        let creds = vec![("a".to_string(), Zeroizing::new("pw".into()))];
        let (blob, salt) = seal("master", &creds);
        assert!(try_unseal("master", &blob, &salt).is_ok());

        assert!(
            try_unseal("wrong", &blob, &salt).is_err(),
            "错误主密码必须解不开"
        );

        for idx in [0usize, NONCE_LEN, blob.len() - 1] {
            let mut bad = blob.clone();
            bad[idx] ^= 0x01;
            assert!(
                try_unseal("master", &bad, &salt).is_err(),
                "改第 {idx} 字节（nonce/密文/tag）必须认证失败"
            );
        }
    }

    /// salt 与 nonce 必须每次都不同——否则相同密码的密文可横向对比，
    /// 也可能被彩虹表命中。这里测的是 create() 用的随机源。
    #[test]
    fn salt_and_nonce_are_fresh_each_time() {
        let s1 = random_bytes::<SALT_LEN>().unwrap();
        let s2 = random_bytes::<SALT_LEN>().unwrap();
        let n1 = random_bytes::<NONCE_LEN>().unwrap();
        let n2 = random_bytes::<NONCE_LEN>().unwrap();
        assert_ne!(s1, s2, "salt 不可复用");
        assert_ne!(n1, n2, "同密钥下 nonce 复用会灾难性失败（GCM 机密性）");
    }

    #[test]
    fn creds_payload_roundtrip() {
        let creds = vec![
            ("堡垒机".to_string(), Zeroizing::new("pw1".into())),
            ("a=b".to_string(), Zeroizing::new("含 = 与 空格".into())),
        ];
        let text = encode_creds(&creds);
        assert_eq!(decode_creds(&text), creds);
    }

    /* ---------------- 重置凭据库 ---------------- */

    /// 测试用的库文件（临时目录，**不碰用户真实凭据库**）。
    fn tmp_vault(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "pp_vault_{tag}_{}_{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        (dir.clone(), dir.join("credentials.vault"))
    }

    /// **动全局状态的测试要串行跑**。
    ///
    /// `MASTER` / `UNLOCKED` 是**进程级**的静态量：解锁 A 的库会把主密码
    /// 记成 A 的，重置 B 的库会 `lock()` 掉解锁状态。Rust 的测试默认
    /// **多线程并行**，两条用例一撞就是"断言看着莫名其妙地挂"。
    static GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 拿全局锁（测试开头调一次）。
    fn global_guard() -> std::sync::MutexGuard<'static, ()> {
        GLOBAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 重置 = 旧主密码失效 + 新主密码可用 + 旧凭据清空。
    ///
    /// 这是"忘了主密码"时唯一的出路，必须钉死三件事：
    /// 1. 旧主密码**真的**解不开了（否则等于没重置）；
    /// 2. 新主密码能开（用户还得能继续用）；
    /// 3. 旧凭据**没了**（用户要重新输一遍——所以 UI 必须提前说清）。
    #[test]
    fn reset_invalidates_old_master_and_clears_creds() {
        let _g = global_guard();
        let (dir, p) = tmp_vault("reset");
        let creds = vec![(
            "堡垒机".to_string(),
            Zeroizing::new("s3cret".to_string()),
        )];
        create_at(&p, "旧主密码", &creds).unwrap();

        // 旧主密码本来能解开
        let got = decrypt_with_at(&p, "旧主密码").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "堡垒机");

        // 重置
        reset_at(&p, "新主密码").unwrap();

        // ① 旧主密码失效（这正是"忘了主密码"的处境）
        assert!(
            decrypt_with_at(&p, "旧主密码").is_err(),
            "重置后旧主密码必须解不开"
        );
        // ② 新主密码能开 ③ 但库是空的
        let got = decrypt_with_at(&p, "新主密码").unwrap();
        assert!(got.is_empty(), "重置后旧凭据必须清空");

        // 原子写不该留下临时文件
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "原子写不该留下临时文件");

        lock();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **空主密码 = 不加密模式**：凭据明文存、文件头明写 protected=false、
    /// 打开不用解锁。
    ///
    /// ⚠️ 关键是要**诚实**：文件里不能看着像密文（其实空串派生出的密钥
    /// 谁都能算出来，一戳就破），所以明写 `protected=false`。
    #[test]
    fn empty_master_means_unprotected() {
        let _g = global_guard();
        let (dir, p) = tmp_vault("empty");
        let creds = vec![("堡垒机".to_string(), Zeroizing::new("pw".into()))];
        create_at(&p, "", &creds).unwrap();

        // 文件头明写没保护
        let kf = read_kf_at(&p).unwrap();
        assert!(!kf_protected(&kf), "空主密码必须是 protected=false");
        assert!(
            kf.string("vault", "salt").is_err(),
            "明文库不该有 salt（没有密钥要派生）"
        );

        // 打开不用解锁：任意密码都能"解锁"（压根没有密码）
        assert!(unlock_at(&p, "").is_ok());
        assert!(is_unlocked());
        assert!(!is_protected_at(&p));
        let got = decrypt_with_at(&p, "").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "堡垒机");

        // 写凭据也能写进去（明文写）
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **明文库解锁时不许把用户敲的密码记成主密码**。
    ///
    /// ⚠️ 这条要是漏了，后果很隐蔽：用户在解锁框里随便敲了个密码（明文库
    /// 本来就不该问他要密码），`MASTER` 记成那个字符串 → 下次写凭据时
    /// `save_all` 用它**把明文库悄悄变成加密库**——用户既没勾过"设主密码"，
    /// 也再也解不开了。
    #[test]
    fn 明文库解锁不记主密码() {
        let _g = global_guard();
        let (dir, p) = tmp_vault("plain");
        let creds = vec![("堡垒机".to_string(), Zeroizing::new("pw".into()))];
        create_at(&p, "", &creds).unwrap();

        // 随便敲一个密码去解锁（明文库压根不该问用户要密码）
        assert!(unlock_at(&p, "用户随手敲的").is_ok());
        // ⚠️ 主密码必须还是**空串**
        assert_eq!(
            master_pw().map(|m| m.to_string()).as_deref(),
            Some(""),
            "明文库的主密码必须是空串，不能记用户敲的字符串"
        );
        // 文件仍然没保护（没被悄悄升级成加密库）
        assert!(!is_protected_at(&p));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **改主密码**：凭据一条不少地搬过去，旧密码失效、新密码能用。
    ///
    /// 这是它跟 `reset` 的根本区别——reset 把凭据**全丢了**，改主密码不能。
    #[test]
    fn change_master_keeps_creds() {
        let _g = global_guard();
        let (dir, p) = tmp_vault("chg");
        let creds = vec![
            ("堡垒机".to_string(), Zeroizing::new("pw1".into())),
            ("测试库".to_string(), Zeroizing::new("pw2".into())),
        ];
        create_at(&p, "旧密码", &creds).unwrap();
        assert!(unlock_at(&p, "旧密码").is_ok());

        // 旧密码错 → 拒绝，且**文件一个字都没动**
        assert!(change_master_at(&p, "错的", "新密码").is_err());
        assert!(unlock_at(&p, "旧密码").is_ok(), "改失败不该动旧库");

        // 改
        change_master_at(&p, "旧密码", "新密码").unwrap();
        assert!(unlock_at(&p, "旧密码").is_err(), "旧密码必须失效");
        assert!(unlock_at(&p, "新密码").is_ok(), "新密码必须能用");
        let got = decrypt_with_at(&p, "新密码").unwrap();
        assert_eq!(got.len(), 2, "凭据一条都不能少");
        assert_eq!(got[0].0, "堡垒机");
        assert_eq!(got[1].0, "测试库");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **明文库升级成加密库**（`old` 传空串——明文库本来就没密码要验），
    /// 以及**不许降级**（新密码留空 → 拒绝）。
    #[test]
    fn change_master_upgrades_plain_but_refuses_downgrade() {
        let _g = global_guard();
        let (dir, p) = tmp_vault("upgrade");
        let creds = vec![("堡垒机".to_string(), Zeroizing::new("pw".into()))];
        create_at(&p, "", &creds).unwrap();
        assert!(!is_protected_at(&p));

        // 降级：已经是明文了，还想更"空"？拒绝。
        assert!(change_master_at(&p, "", "").is_err());

        // 升级：明文 → 加密，凭据原样保留
        change_master_at(&p, "", "新密码").unwrap();
        assert!(is_protected_at(&p));
        assert!(unlock_at(&p, "新密码").is_ok());
        let got = decrypt_with_at(&p, "新密码").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "堡垒机");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
