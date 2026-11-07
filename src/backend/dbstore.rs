//! 数据库连接配置持久化（7.2.7 / 阶段 10）：`~/.config/puttyplus/databases.ini`。
//!
//! 只存非敏感字段（连接名/类型/地址/端口/用户/库/是否记住密码）；密码进
//! 凭据库（`vault`），键名 `db:<会话键>/<连接名>`，不落 ini、不进 argv/日志。
//!
//! **一个 SSH 会话下可以有多个数据库连接**（阶段 10）：ini 里会话键是父组，
//! 每个数据库连接是它的**子组**：
//!
//! ```ini
//! [root@10.0.0.1:22]
//! count=2
//!
//! [root@10.0.0.1:22/本地 PG]
//! proto=pg
//! host=127.0.0.1
//! port=5432
//! user=postgres
//! dbname=postgres
//! remember=true
//!
//! [root@10.0.0.1:22/测试库 MySQL]
//! proto=mysql
//! host=127.0.0.1
//! port=3306
//! user=root
//! dbname=demo
//! ```
//!
//! 连接之间靠**连接名**区分（用户可自己命名，见侧边栏的会话树）。
//!
//! **向后兼容**：老版本一个会话只有一份配置，直接写在会话组里
//! （`[root@h:22]` 下面就是 `proto=…`）。读的时候如果会话组自己带
//! `proto`，就当成一条"没有名字"的老配置——升级不会把用户的配置弄丢。

use std::path::PathBuf;

use glib::KeyFile;

use super::db::DbProto;
use crate::args::ConnOptions;

/// 一个数据库连接配置。
#[derive(Debug, Clone, PartialEq)]
pub struct DbProfile {
    /// 连接名（用户可命名）；同一 SSH 会话下靠它区分多个连接。
    /// 空 = 老版本遗留的那一份（没名字）。
    pub name: String,
    pub proto: DbProto,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub dbname: String,
    /// 用户勾了「记住密码」；密码本身在凭据库里
    pub remember: bool,
}

impl DbProfile {
    /// 没填连接名时自动生成一个（省得用户每回都自己起名）。
    ///
    /// 形如 `pg 127.0.0.1:5432/postgres`——类型 + 地址 + 库，一眼能分清。
    pub fn default_name(proto: DbProto, host: &str, port: u16, dbname: &str) -> String {
        let db = if dbname.is_empty() { "" } else { dbname };
        format!("{} {}:{}/{}", proto.as_str(), host, port, db)
    }

    /// 一行摘要（侧边栏 / 状态栏用）。
    pub fn summary(&self) -> String {
        format!(
            "{} {}:{}/{}",
            self.proto.as_str(),
            self.host,
            self.port,
            self.dbname
        )
    }
}

/// 会话标识：会话名优先，否则 `user@host:port`（与连接管理器语义一致，
/// 重启后仍然稳定）。
pub fn session_key(o: &ConnOptions) -> String {
    if let Some(n) = o.session_name.as_deref().filter(|s| !s.is_empty()) {
        return format!("s:{n}");
    }
    format!("{}@{}:{}", o.user, o.host, o.port)
}

/// 凭据库里的名字 → `(会话键, 连接名)`。
///
/// **给"凭据库管理器"用**：用户想看库里存了些什么，光看 `db:root@h:22/postgres`
/// 这一坨不好读，拆成"哪个会话的哪条连接"就清楚了。
///
/// - 新格式 `db:<会话键>/<连接名>` → `(会话键, 连接名)`；
/// - 老格式 `db:<会话键>`（一个会话一份配置）→ `(会话键, "")`；
/// - 不带 `db:` 前缀的（理论上没有）→ 整体当会话键。
pub fn split_vault_name(full: &str) -> (String, String) {
    let body = full.strip_prefix("db:").unwrap_or(full);
    match body.split_once('/') {
        Some((k, n)) => (k.to_string(), n.to_string()),
        None => (body.to_string(), String::new()),
    }
}

/// 凭据库里的键名（避免和连接密码重名）。
///
/// 带连接名 → `db:<会话键>/<连接名>`；老版本（一个会话一份配置）的键是
/// `db:<会话键>`，读的时候两个都试（新的优先）。
pub fn vault_name(key: &str, name: &str) -> String {
    if name.is_empty() {
        format!("db:{key}")
    } else {
        format!("db:{key}/{name}")
    }
}

/// 老版本的凭据库键（一个会话一份配置）。
pub fn legacy_vault_name(key: &str) -> String {
    format!("db:{key}")
}

fn ini_path() -> PathBuf {
    // 环境变量可覆盖：测试与调试时想看/改别的文件，不必动真实配置。
    if let Some(p) = std::env::var_os("PUTTYPLUS_DB_INI") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir()
        .join("puttyplus")
        .join("databases.ini")
}

/// KeyFile 组名不能含 `[` `]` 与换行；会话名可能带这些字符。
fn group(key: &str) -> String {
    key.chars()
        .map(|c| match c {
            '[' | ']' | '\n' | '\r' => '_',
            _ => c,
        })
        .collect()
}

/// 子组名（`<会话组>/<连接名>`）。
fn child_group(key: &str, name: &str) -> String {
    format!("{}/{}", group(key), group(name))
}

/* ---------------- 读 ---------------- */

/// 列出会话下的所有数据库连接（按 ini 里的顺序）。
pub fn list(key: &str) -> Vec<DbProfile> {
    list_from(&ini_path(), key)
}

fn list_from(path: &std::path::Path, key: &str) -> Vec<DbProfile> {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return Vec::new();
    }
    let g = group(key);
    let mut out: Vec<DbProfile> = Vec::new();
    // 子组 = 正式的多连接条目
    for gname in kf.groups() {
        if !gname.starts_with(&format!("{g}/")) {
            continue;
        }
        let name = gname[g.len() + 1..].to_string();
        if let Some(p) = profile_from(&kf, &gname, &name) {
            out.push(p);
        }
    }
    // 老格式：会话组自己带 proto（升级前写的那一份）
    if out.is_empty() {
        if let Some(p) = profile_from(&kf, &g, "") {
            out.push(p);
        }
    }
    out
}

/// 取指定名字的连接。
pub fn get(key: &str, name: &str) -> Option<DbProfile> {
    get_from(&ini_path(), key, name)
}

fn get_from(path: &std::path::Path, key: &str, name: &str) -> Option<DbProfile> {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return None;
    }
    if !name.is_empty() {
        let g = child_group(key, name);
        if kf.has_group(&g) {
            return profile_from(&kf, &g, name);
        }
    }
    // 名字对不上就退回第一条（用户改名后老配置还在）
    list_from(path, key).into_iter().next()
}

/// 会话下的第一个连接（打开数据库窗口时默认预填它）。
pub fn first(key: &str) -> Option<DbProfile> {
    list(key).into_iter().next()
}

/// 从 KeyFile 的一个组里读出一份配置。
fn profile_from(kf: &KeyFile, g: &str, name: &str) -> Option<DbProfile> {
    if !kf.has_group(g) {
        return None;
    }
    let proto = kf
        .string(g, "proto")
        .ok()
        .and_then(|s| DbProto::from_str(&s))
        .unwrap_or(DbProto::Postgres);
    let host = kf.string(g, "host").ok()?.to_string();
    if host.is_empty() {
        return None;
    }
    let port = kf
        .string(g, "port")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| proto.default_port());
    let user = kf.string(g, "user").ok().unwrap_or_default().to_string();
    let dbname = kf.string(g, "dbname").ok().unwrap_or_default().to_string();
    let remember = kf.boolean(g, "remember").unwrap_or(false);
    Some(DbProfile {
        name: name.to_string(),
        proto,
        host,
        port,
        user,
        dbname,
        remember,
    })
}

/* ---------------- 写 ---------------- */

/// 存一个连接（按连接名覆盖；同名就是更新，不同名就是新增）。
pub fn save(key: &str, p: &DbProfile) -> bool {
    save_to(&ini_path(), key, p)
}

fn save_to(path: &std::path::Path, key: &str, p: &DbProfile) -> bool {
    let before = std::fs::read_to_string(path).unwrap_or_default();
    if let Some(dir) = path.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
        let _ = crate::platform::perm::set_private_dir(dir);
    }
    let kf = KeyFile::new();
    let _ = kf.load_from_file(path, glib::KeyFileFlags::NONE);
    let parent = group(key);
    // 会话组里记一份数量（给人看的，也方便将来直接读）
    let count = kf
        .groups()
        .iter()
        .filter(|x| x.starts_with(&format!("{parent}/")))
        .count();
    kf.set_string(&parent, "count", &(count.max(1)).to_string());

    // **老格式迁移**：升级前一个会话只有一份配置，直接写在会话组里
    // （`[key]` 下面就是 proto=…）。现在配置住子组，老的那份得搬过去——
    // 不搬的话它既不在子组里（读不到），又还占着会话组（同一条连接出现两次）。
    if kf.has_key(&parent, "proto").unwrap_or(false) {
        if let Some(old) = profile_from(&kf, &parent, "") {
            let same = old.proto == p.proto
                && old.host == p.host
                && old.port == p.port
                && old.user == p.user
                && old.dbname == p.dbname;
            if !same {
                // 内容不同 → 当成另一条连接保留下来（别把用户的配置弄丢）
                let old_name = DbProfile::default_name(old.proto, &old.host, old.port, &old.dbname);
                write_profile(&kf, &child_group(key, &old_name), &old);
            }
        }
        for k in ["proto", "host", "port", "user", "dbname", "remember"] {
            let _ = kf.remove_key(&parent, k);
        }
    }

    write_profile(&kf, &child_group(key, &p.name), p);
    let data = kf.to_data();
    // 原子写：同目录临时文件 + rename。两个标签同时连库（同一会话键）时
    // 直接 write 会因文件偏移交错把内容写重，rename 保证读到的永远是完整文件。
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "databases.ini".into());
    let tmp = path.with_file_name(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    if std::fs::write(&tmp, data.as_bytes()).is_err() {
        return false;
    }
    let _ = crate::platform::perm::set_private_file(&tmp);
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    let _ = before;
    true
}

/// 把一份配置写进 KeyFile 的一个组。
fn write_profile(kf: &KeyFile, g: &str, p: &DbProfile) {
    kf.set_string(g, "proto", p.proto.as_str());
    kf.set_string(g, "host", &p.host);
    kf.set_string(g, "port", &p.port.to_string());
    kf.set_string(g, "user", &p.user);
    kf.set_string(g, "dbname", &p.dbname);
    if p.remember {
        kf.set_boolean(g, "remember", true);
    } else {
        let _ = kf.remove_key(g, "remember");
    }
}

/// 删掉一个连接（按名字）。返回是否真的删掉了。
pub fn remove(key: &str, name: &str) -> bool {
    remove_from(&ini_path(), key, name)
}

fn remove_from(path: &std::path::Path, key: &str, name: &str) -> bool {
    let kf = KeyFile::new();
    if kf.load_from_file(path, glib::KeyFileFlags::NONE).is_err() {
        return false;
    }
    let parent = group(key);
    let g = child_group(key, name);
    if !kf.has_group(&g) {
        return false;
    }
    let _ = kf.remove_group(&g);
    let count = kf
        .groups()
        .iter()
        .filter(|x| x.starts_with(&format!("{parent}/")))
        .count();
    kf.set_string(&parent, "count", &count.to_string());
    let data = kf.to_data();
    std::fs::write(path, data.as_bytes()).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ini(tag: &str) -> std::path::PathBuf {
        // ⚠ 文件放在一个临时目录里，**进程退出时整块删**（见
        // `util::testutil`）—— 以前这些 .ini 直接落在 `target/` 里，
        // 每次跑测试都多留一批。
        let dir = crate::util::testutil::temp_dir_in(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"),
            &format!("dbstore-tests-{}-{tag}", std::process::id()),
        );
        dir.join(format!("{tag}.ini"))
    }

    fn profile(name: &str, port: u16) -> DbProfile {
        DbProfile {
            name: name.to_string(),
            proto: DbProto::Postgres,
            host: "127.0.0.1".into(),
            port,
            user: "postgres".into(),
            dbname: "postgres".into(),
            remember: true,
        }
    }

    #[test]
    fn session_key_uses_name_then_address() {
        let mut o = ConnOptions::default();
        o.host = "h".into();
        o.user = "u".into();
        o.port = 2222;
        assert_eq!(session_key(&o), "u@h:2222");
        o.session_name = Some("prod".into());
        assert_eq!(session_key(&o), "s:prod");
    }

    #[test]
    fn group_name_is_keyfile_safe() {
        assert_eq!(group("a[b]c"), "a_b_c");
        assert_eq!(group("a[b]c"), group("a[b]c"));
    }

    #[test]
    fn one_session_keeps_multiple_profiles() {
        let path = tmp_ini("multi");
        let _ = std::fs::remove_file(&path);
        assert!(save_to(&path, "u@h:22", &profile("本地 PG", 5432)));
        assert!(save_to(&path, "u@h:22", &profile("测试 MySQL", 3306)));
        let list = list_from(&path, "u@h:22");
        assert_eq!(list.len(), 2, "{list:?}");
        assert_eq!(list[0].name, "本地 PG");
        assert_eq!(list[0].port, 5432);
        assert_eq!(list[1].name, "测试 MySQL");
        assert_eq!(list[1].port, 3306);
        // 换个会话键看不到别人的
        assert!(list_from(&path, "other@h:22").is_empty());
        // 按名字取
        assert_eq!(get_from(&path, "u@h:22", "测试 MySQL").unwrap().port, 3306);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn saving_same_name_updates_in_place() {
        let path = tmp_ini("update");
        let _ = std::fs::remove_file(&path);
        assert!(save_to(&path, "u@h:22", &profile("本地 PG", 5432)));
        let mut p2 = profile("本地 PG", 5433);
        p2.dbname = "other".into();
        assert!(save_to(&path, "u@h:22", &p2));
        let list = list_from(&path, "u@h:22");
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].port, 5433);
        assert_eq!(list[0].dbname, "other");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn remove_drops_one_profile_only() {
        let path = tmp_ini("remove");
        let _ = std::fs::remove_file(&path);
        assert!(save_to(&path, "u@h:22", &profile("A", 5432)));
        assert!(save_to(&path, "u@h:22", &profile("B", 3306)));
        assert!(remove_from(&path, "u@h:22", "A"));
        let list = list_from(&path, "u@h:22");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "B");
        assert!(!remove_from(&path, "u@h:22", "不存在"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_single_profile_is_still_readable() {
        // 升级前一个会话只有一份配置，直接写在会话组里
        let path = tmp_ini("legacy");
        std::fs::write(
            &path,
            "[u@h:22]\nproto=pg\nhost=h\nport=1111\nuser=u\ndbname=d\n",
        )
        .unwrap();
        let list = list_from(&path, "u@h:22");
        assert_eq!(list.len(), 1, "{list:?}");
        assert_eq!(list[0].host, "h");
        assert_eq!(list[0].port, 1111);
        assert_eq!(list[0].name, "");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn legacy_profile_is_migrated_on_save() {
        // 老配置 + 新保存一条 → 两条都在（老的那条名字为空）
        let path = tmp_ini("migrate");
        std::fs::write(
            &path,
            "[u@h:22]\nproto=pg\nhost=h\nport=1111\nuser=u\ndbname=d\n",
        )
        .unwrap();
        assert!(save_to(&path, "u@h:22", &profile("新连接", 5432)));
        let list = list_from(&path, "u@h:22");
        assert_eq!(list.len(), 2, "{list:?}");
        assert!(list.iter().any(|p| p.name == "新连接"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn manual_file_is_migrated_into_subgroups() {
        // 升级前手写的 ini：一个会话一份配置，直接写在会话组里
        let path = tmp_ini("manual");
        std::fs::write(
            &path,
            "[g]\nproto=pg\nhost=h\nport=1111\nuser=u\ndbname=d\n",
        )
        .unwrap();
        let p = DbProfile {
            name: "本地 PG".into(),
            proto: DbProto::Postgres,
            host: "h".into(),
            port: 2222,
            user: "u".into(),
            dbname: "d".into(),
            remember: true,
        };
        assert!(save_to(&path, "g", &p));
        // 老的那份内容不同 → 保留成一条子组；新的也是一条子组
        let list = list_from(&path, "g");
        assert_eq!(list.len(), 2, "{list:?}");
        assert!(list.iter().any(|x| x.port == 1111), "{list:?}");
        assert!(list.iter().any(|x| x.port == 2222 && x.name == "本地 PG"), "{list:?}");
        // 会话组里不再留 proto（不然同一条连接会读出两次）
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("[g]\nproto"), "{text}");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn profile_roundtrip_in_temp_ini() {
        // 直接测 KeyFile 序列化，不碰用户真实配置文件
        let kf = KeyFile::new();
        let g = group("u@h:22");
        kf.set_string(&g, "proto", "mysql");
        kf.set_string(&g, "host", "127.0.0.1");
        kf.set_string(&g, "port", "3306");
        kf.set_boolean(&g, "remember", true);
        let data = kf.to_data();
        let kf2 = KeyFile::new();
        kf2.load_from_data(&data, glib::KeyFileFlags::NONE).unwrap();
        assert_eq!(kf2.string(&g, "proto").unwrap(), "mysql");
        assert!(kf2.boolean(&g, "remember").unwrap());
    }

    #[test]
    fn vault_name_distinguishes_profiles() {
        assert_eq!(vault_name("u@h:22", "本地 PG"), "db:u@h:22/本地 PG");
        // 老版本（一个会话一份配置）的键
        assert_eq!(vault_name("u@h:22", ""), "db:u@h:22");
        assert_eq!(legacy_vault_name("u@h:22"), "db:u@h:22");
    }

    #[test]
    fn default_name_is_readable() {
        assert_eq!(
            DbProfile::default_name(DbProto::Postgres, "127.0.0.1", 5432, "postgres"),
            "pg 127.0.0.1:5432/postgres"
        );
    }
}
