//! 连接**存储层**（对应 C 版 connmgr.c）：自建连接持久化到
//! `~/.config/puttyplus/connections.ini`（glib KeyFile，权限 600），
//! 与 `~/.ssh/config` 只读别名合并。
//!
//! ⚠️ **这里不再有"连接管理器窗口"**（阶段 77b）：审计 P1-1 说侧栏与连接
//! 管理器是两套完整 CRUD，用户不知道谁是主入口；用户 2026-10-31 拍板
//! 「彻底删掉，把所有功能都放到侧栏」⇒ 窗口、工具栏、右键菜单、树的
//! 构造代码全删了，**只剩存储层**：条目读写、导入导出 PuTTY 会话、
//! 端口转发规则、认证选项。
//!
//! 与 C 版的差异（计划内修复）：spawn 新窗口时密码不再放进 argv
//! （ps 可见），改用 `PUTTYPLUS_PW_SESSION` 环境变量传递——仅同用户可读。

use std::path::PathBuf;
use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use zeroize::Zeroizing;

use crate::args::ConnOptions;
use crate::backend::forward;
use crate::config::{list_ssh_aliases, resolve_ssh_config_alias};
use crate::util::atoi_like;
use crate::i18n::tr;
use crate::trf;

/* ---------------- 存储层 ---------------- */

#[derive(Clone)]
pub struct ConnEntry {
    pub name: String,
    pub host: String,
    pub user: Option<String>,
    pub password: Option<Zeroizing<String>>,
    /// 0 = 未指定
    pub port: i32,
    /// true=自建；false=~/.ssh/config 只读别名
    pub custom: bool,
    /// "ssh"（默认）/ "telnet" / "serial"
    pub proto: String,
    /// 串口配置（proto == "serial" 时用；PuTTY sercfg 格式）
    pub sercfg: Option<String>,
    /// 端口转发规则（1.4：connections.ini 的 forwardings 键，
    /// 与 PuTTY PortForwardings 双向互转；停用项 enabled=false）
    pub forwards: Vec<forward::FwdRule>,
    /// **进程内**端口映射规则（connections.ini 的 inprocess_forwardings 键）。
    ///
    /// 与 `forwards` 的分工：`forwards` 交给终端 spawn 的 ssh/plink 承接；
    /// 这里跑在**应用自己那条 SSH 连接**上（不新建 SSH 连接、终端关了
    /// 照样在）。堡垒机场景要的正是后者。
    pub inproc_forwards: Vec<forward::FwdRule>,
    /// **这条连接的会话默认值**（连接编辑器的「会话」页，阶段 69 之二）。
    ///
    /// ⚠️ 与「会话选项」的区别：那是**运行时**改一个会话（存在
    /// `settings.ini` 的会话段）；这里是**连接自带**的默认值（存在
    /// `connections.ini` 的条目段），这条连接**新开的会话**都用它。
    pub sess: SessionOpts,
    /// 私钥文件（阶段 70，审计 P1-5）。`None` = 不指定。
    ///
    /// ⚠️ 存的是**绝对路径**（`~/.ssh` 的按绝对路径存，省得换台机器
    /// 就指错地方）；终端侧会拿它拼 `-i`，进程内 SFTP 侧用它做公钥认证。
    pub keyfile: Option<String>,
    /// 认证方式（阶段 70）：允许用哪些凭据。
    pub auth_method: crate::args::AuthMethod,
}

/// 连接级的**会话默认设置**（连接编辑器的「会话」页）。
///
/// ⚠️ 全部字段都是 `Option`：**没设 = 用内置默认**，这样 ini 里不会
/// 塞一堆与默认值一样的键（C 版忽略未知键，但少写一个键少一份歧义）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionOpts {
    /// 保活间隔（秒）；`None` = 默认 30，`Some(0)` = 关闭保活。
    pub keepalive_sec: Option<u32>,
    /// 终端类型（TERM）；`None` = 默认 xterm-256color。
    pub term_type: Option<String>,
    /// 认证优先级；`None` = 默认（密码优先，与 C 版一致）。
    pub auth_order: Option<crate::args::AuthOrder>,
    /// 终端字号；`None` = 用终端内置的 10.5（首选项中心的全局默认会兜底）。
    pub font_size: Option<f64>,
    /// 终端响铃；`None` = 默认。
    pub bell: Option<bool>,
}

impl ConnEntry {
    /// 条目 → 连接参数。侧边栏与各处共用这一处。
    ///
    /// proto 和 sercfg 必须一起搬：分开搬过一次，结果是串口条目按 SSH
    /// 去连（host 字段被当成主机名，设备路径直接连不上）。
    /// 手搓第二份转换就会重犯，所以只留这一个入口。
    pub fn to_opts(&self) -> ConnOptions {
        let mut o = ConnOptions {
            host: self.host.clone(),
            port: if self.port > 0 { self.port } else { 22 },
            proto: if self.proto.is_empty() {
                "ssh".into()
            } else {
                self.proto.clone()
            },
            ..Default::default()
        };
        if let Some(u) = &self.user {
            if !u.is_empty() {
                o.user = u.clone();
            }
        }
        if let Some(p) = &self.password {
            if !p.is_empty() {
                o.password = Some(p.clone());
            }
        }
        // ⚠️ **凭证库兜底**（阶段 C12）：手里的 ConnEntry 是**快照**——侧栏
        //    的列表是启动/上次刷新时读的，那时凭证库多半还锁着，而保存
        //    密码时 ini 的明文键已删（进了库）⇒ 快照里 password 恒为 None，
        //    用户解锁了库再连还是要输密码（用户 2026-11-09 报）。
        //    组装连接参数的**这一刻**现查凭证库：解锁了就能拿到（键 =
        //    连接名，`save_conn` 存的时候用的同一个键）；锁着 get 返回
        //    None，走原路（弹密码框）。ini 明文优先于库（上面已赋值的
        //    不覆盖）。
        if o.password.is_none() && !self.name.is_empty() {
            o.password = crate::vault::get(&self.name);
        }
        if !self.name.is_empty() {
            o.title = Some(self.name.clone());
        }
        if self.custom {
            o.session_name = Some(self.name.clone());
        }
        // sercfg 是串口专用：SSH 条目里若残留了同名字段也一律不搬，
        // 免得把串口参数当别的什么东西用
        if o.proto.eq_ignore_ascii_case("serial") {
            if let Some(c) = &self.sercfg {
                let conf = crate::args::parse_sercfg(c);
                o.serial_baud = conf.baud;
                o.serial_data = conf.data;
                o.serial_parity = conf.parity;
                o.serial_stop = conf.stop;
                o.serial_flow = conf.flow;
            }
        }
        // 会话默认值（连接编辑器的「会话」页，阶段 69 之二）：
        // 没设的项保持 ConnOptions 的内置默认。
        if let Some(k) = self.sess.keepalive_sec {
            o.keepalive_sec = k;
        }
        if let Some(t) = &self.sess.term_type {
            o.term_type = Some(t.clone());
        }
        if let Some(a) = self.sess.auth_order {
            o.auth_order = a;
        }
        if let Some(f) = self.sess.font_size {
            o.font_size = Some(f);
        }
        if let Some(b) = self.sess.bell {
            o.bell = Some(b);
        }
        // 私钥与认证方式（阶段 70）
        if let Some(kf) = &self.keyfile {
            if !kf.is_empty() {
                o.keyfile = Some(kf.clone());
            }
        }
        o.auth_method = self.auth_method;
        // 进程内端口映射：只搬启用的（停用项保留在连接配置里，
        // 在连接配置里改）
        o.inproc_forwards = self
            .inproc_forwards
            .iter()
            .filter(|r| r.enabled)
            .map(|r| format!("{}:{}", r.kind, r.spec))
            .collect();
        o
    }
}

fn ini_path() -> PathBuf {
    // 环境变量可覆盖：测试与调试时想看/改别的文件，不必动真实配置。
    if let Some(p) = std::env::var_os("PUTTYPLUS_CONN_INI") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir().join("puttyplus").join("connections.ini")
}

fn ini_load() -> glib::KeyFile {
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(ini_path(), glib::KeyFileFlags::NONE);
    kf
}

fn ini_load_from(path: &std::path::Path) -> glib::KeyFile {
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(path, glib::KeyFileFlags::NONE);
    kf
}

fn ini_save(kf: &glib::KeyFile) -> bool {
    ini_save_to(kf, &ini_path())
}

fn ini_save_to(kf: &glib::KeyFile, path: &std::path::Path) -> bool {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            if std::fs::create_dir_all(dir).is_err() {
                return false;
            }
            let _ = crate::platform::perm::set_private_dir(dir);
        }
    }
    let data = kf.to_data();
    let ok = std::fs::write(path, data.as_bytes()).is_ok();
    if ok {
        // 可能存有明文密码
        let _ = crate::platform::perm::set_private_file(path);
    }
    ok
}

/// ssh config **别名**的进程内转发规则存在 ini 的这一段里（键 = 别名）。
///
/// ⚠️ **为什么不写进 ~/.ssh/config**：那是 ssh 客户端自己的配置，里面是
/// ssh 的 `LocalForward`（交给 ssh 进程做转发），跟本应用的**进程内**
/// 转发（跑在应用自己那条 SSH 连接上）不是一回事，混在一起会互相干扰；
/// 而且改用户的 ssh 配置文件风险大，别名里还可能有通配符。
///
/// 用**别名**当键（不是会话键）：会话键是展开后的 `user@host:port`，
/// 两个别名指向同一台机器时会撞；别名本身是稳定标识，下次从这个别名
/// 连接时按名字取回规则。
pub const ALIAS_FWD_GROUP: &str = "ssh_alias_forwards";

/// 全部条目：自建连接在前（文件序），ssh config 别名在后。
pub fn load_all() -> Vec<ConnEntry> {
    load_all_with(&ini_path(), &list_ssh_aliases())
}

/// 全部条目（路径与别名表可注入，测试用——别去读用户真实的 ~/.ssh/config）。
fn load_all_with(path: &std::path::Path, aliases: &[crate::config::SshHostAlias]) -> Vec<ConnEntry> {
    let mut out = load_custom_from(path);
    // 别名的进程内转发规则存在 ini 的单独一段里（键 = 别名）
    let kf = ini_load_from(path);
    for a in aliases {
        let alias_fwd =
            forward::parse_rules(&kf.string(ALIAS_FWD_GROUP, &a.alias).unwrap_or_default());
        out.push(ConnEntry {
            name: a.alias.clone(),
            host: a.alias.clone(),
            user: a.user.clone(),
            password: None,
            port: a.port,
            custom: false,
            proto: "ssh".into(),
            sercfg: None,
            forwards: Vec::new(),
            inproc_forwards: alias_fwd,
            // 别名条目没有自己的「会话」设置（走 ~/.ssh/config 与默认）
            sess: SessionOpts::default(),
            // 别名条目的私钥/认证方式也在 ~/.ssh/config 里
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        });
    }
    out
}

/// 从 ini 的一段里读**会话默认设置**（阶段 69 之二）。
///
/// ⚠️ 缺键 = 用内置默认（`None`）——不往 ini 里塞与默认值一样的键。
fn read_session_opts(kf: &glib::KeyFile, group: &str) -> SessionOpts {
    SessionOpts {
        keepalive_sec: kf
            .uint64(group, "keepalive")
            .ok()
            .map(|v| v as u32),
        term_type: kf
            .string(group, "term")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
        // ⚠️ 键名是 `authorder` 而不是 `auth`：`auth` 那把键归**认证方式**
        // （阶段 70）了，两把键在同一个组里，撞名就互相覆盖。
        auth_order: kf
            .string(group, "authorder")
            .ok()
            .and_then(|s| crate::args::AuthOrder::parse(&s)),
        font_size: kf.string(group, "font").ok().and_then(|s| {
            // 范围与界面一致（6..=48）：手改 ini 写进离谱的字号会让终端没法看
            s.trim()
                .parse::<f64>()
                .ok()
                .filter(|v| (6.0..=48.0).contains(v))
        }),
        bell: kf.boolean(group, "bell").ok(),
    }
}

/// 只改**会话默认设置**那几把键（阶段 69 之二）：局部回写。
///
/// ⚠️ **不能**走 `save_entry` 的整条覆盖——那条会把没传的字段当成"删掉"
/// （转发规则、串口参数都靠这个语义）。
pub fn save_session_opts(name: &str, opts: &SessionOpts) -> bool {
    save_session_opts_to(&ini_path(), name, opts)
}

fn save_session_opts_to(path: &std::path::Path, name: &str, opts: &SessionOpts) -> bool {
    let kf = ini_load_from(path);
    let Some(target) = group_of_name(&kf, name) else {
        return false;
    };
    let mut set_or_del = |key: &str, v: Option<String>| match v {
        Some(v) => {
            kf.set_string(&target, key, &v);
        }
        None => {
            let _ = kf.remove_key(&target, key);
        }
    };
    set_or_del(
        "keepalive",
        opts.keepalive_sec.map(|v| v.to_string()),
    );
    set_or_del("term", opts.term_type.clone());
    set_or_del(
        "authorder",
        opts.auth_order.map(|a| a.as_str().to_string()),
    );
    set_or_del("font", opts.font_size.map(|v| format!("{v:.1}")));
    set_or_del("bell", opts.bell.map(|b| if b { "1" } else { "0" }.to_string()));
    ini_save_to(&kf, path)
}

/// 按 `name` 找 ini 里的组名（连接条目靠 name 认，不是靠组名）。
fn group_of_name(kf: &glib::KeyFile, name: &str) -> Option<String> {
    for g in kf.groups() {
        if kf.string(&g, "name").ok().as_deref() == Some(name) {
            return Some(g.to_string());
        }
    }
    None
}

/// 只改**私钥与认证方式**那两把键（阶段 70）：局部回写。
///
/// ⚠️ 与 `save_session_opts` 同一个理由：不能走 `save_entry` 的整条覆盖。
/// ⚠️ 认证方式是「自动」（默认）时**不落盘**——缺键 = 自动，ini 里不留
/// 一堆"跟默认一样"的值。
pub fn save_auth_opts(
    name: &str,
    keyfile: Option<&str>,
    method: crate::args::AuthMethod,
) -> bool {
    save_auth_opts_to(&ini_path(), name, keyfile, method)
}

fn save_auth_opts_to(
    path: &std::path::Path,
    name: &str,
    keyfile: Option<&str>,
    method: crate::args::AuthMethod,
) -> bool {
    let kf = ini_load_from(path);
    let Some(target) = group_of_name(&kf, name) else {
        return false;
    };
    let mut set_or_del = |key: &str, v: Option<String>| match v {
        Some(v) => {
            kf.set_string(&target, key, &v);
        }
        None => {
            let _ = kf.remove_key(&target, key);
        }
    };
    set_or_del(
        "keyfile",
        keyfile
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    );
    set_or_del(
        "auth",
        match method {
            crate::args::AuthMethod::Auto => None,
            m => Some(m.as_str().to_string()),
        },
    );
    ini_save_to(&kf, path)
}

/// 仅自建连接（按 ini 文件序）。
fn load_custom_from(path: &std::path::Path) -> Vec<ConnEntry> {
    let mut out = Vec::new();
    let kf = ini_load_from(path);
    for g in kf.groups() {
        let name = kf.string(&g, "name").ok().filter(|s| !s.is_empty());
        let host = kf.string(&g, "host").ok().filter(|s| !s.is_empty());
        let (Some(name), Some(host)) = (name, host) else {
            continue;
        };
        // 密码来源优先级：凭据库（已解锁）> ini 明文（迁移前的旧库）。
        // 未解锁时 vault 返回 None，明文仍可用——不会因为加了凭据库
        // 就让没设过主密码的用户连不上。
        let pw_ini = kf
            .string(&g, "password")
            .ok()
            .map(|s| Zeroizing::new(s.to_string()));
        let password = if crate::vault::is_unlocked() {
            crate::vault::get(name.as_str()).or(pw_ini)
        } else {
            pw_ini
        };
        out.push(ConnEntry {
            name: name.to_string(),
            host: host.to_string(),
            user: kf.string(&g, "user").ok().map(|s| s.to_string()),
            password,
            port: kf.uint64(&g, "port").unwrap_or(0) as i32,
            custom: true,
            proto: kf.string(&g, "proto").ok().map(|s| s.to_string()).unwrap_or_else(|| "ssh".into()),
            sercfg: kf.string(&g, "sercfg").ok().map(|s| s.to_string()),
            forwards: forward::parse_rules(&kf.string(&g, "forwardings").unwrap_or_default()),
            inproc_forwards: forward::parse_rules(
                &kf.string(&g, "inprocess_forwardings").unwrap_or_default(),
            ),
            sess: read_session_opts(&kf, &g),
            keyfile: kf
                .string(&g, "keyfile")
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty()),
            auth_method: kf
                .string(&g, "auth")
                .ok()
                .and_then(|s| crate::args::AuthMethod::parse(&s))
                .unwrap_or_default(),
        });
    }
    out
}

#[allow(clippy::too_many_arguments)]
pub fn save_entry(
    name: &str,
    host: &str,
    port: i32,
    user: Option<&str>,
    password: Option<&str>,
    proto: Option<&str>,
    sercfg: Option<&str>,
    forwards: Option<&str>,
) -> bool {
    save_entry_to(
        &ini_path(),
        name,
        host,
        port,
        user,
        password,
        proto,
        sercfg,
        forwards,
    )
}

/// 删除 ini 里所有明文 password 键（迁移到凭据库后调用）。
///
/// 只动 password 键，**不重写条目**——重写会把 forwardings / sercfg
/// 这类"没传就等于删"的字段顺手抹掉（save_entry 的 None 语义是删除）。
pub fn purge_ini_passwords() -> bool {
    let p = ini_path();
    let mut kf = ini_load();
    let groups: Vec<String> = kf.groups().into_iter().map(|g| g.to_string()).collect();
    let mut removed = 0;
    for g in groups {
        if kf.has_key(&g, "password").unwrap_or(false) && kf.remove_key(&g, "password").is_ok() {
            removed += 1;
        }
    }
    removed > 0 && ini_save(&kf)
}

/// 只改 inprocess_forwardings 键（进程内端口映射）：与 save_forwards
/// 同样走局部回写——整条覆盖会把并发修改抹掉。
pub fn save_inproc_forwards(name: &str, forwards: &str) -> bool {
    save_inproc_forwards_to(&ini_path(), name, forwards)
}

fn save_inproc_forwards_to(path: &std::path::Path, name: &str, forwards: &str) -> bool {
    let mut kf = ini_load_from(path);
    let mut target = None;
    for g in kf.groups() {
        if kf.string(&g, "name").ok().as_deref() == Some(name) {
            target = Some(g.to_string());
            break;
        }
    }
    let Some(target) = target else { return false };
    let rules = forward::parse_rules(forwards);
    if rules.is_empty() {
        let _ = kf.remove_key(&target, "inprocess_forwardings");
    } else {
        kf.set_string(
            &target,
            "inprocess_forwardings",
            &forward::to_storage_value(&rules),
        );
    }
    ini_save_to(&kf, path)
}

/// 转发规则该往哪写（转发窗口「保存到连接配置」用）。
#[derive(Debug, Clone, PartialEq)]
pub enum FwdSaveTarget {
    /// 自建条目：写进它在 connections.ini 里的那个组（键 `inprocess_forwardings`）
    Custom(String),
    /// ssh config 别名：写进 `[ssh_alias_forwards]` 段，键 = 别名
    Alias(String),
}

/// 找不到可写回的目标时，说明**实际**原因（文案要对得上，别再笼统地
/// 说「不在自定义连接里」——ssh 别名明明就在列表里，只是只读）。
#[derive(Debug, Clone, PartialEq)]
pub enum FwdTargetErr {
    /// 这条会话不是从任何条目连上的（命令行直接开的）
    NoEntry,
    /// 有多个 ssh 别名指向同一台机器，说不清规则该存哪一条
    Ambiguous(Vec<String>),
}

/// 按**会话键**找转发规则该写到哪。
///
/// ⚠️ **自建条目和 ssh 别名要分开处理**：
/// - 自建条目的会话键是 `s:<名字>`（不受别名展开影响），规则存在它
///   自己的 ini 组里；
/// - ssh 别名的会话键是展开后的 `user@host:port`，规则存在
///   `[ssh_alias_forwards]` 段里（键 = 别名）。
///
/// `title_hint` = 会话标题（= 条目名）：多个别名指向同一台机器时，
/// 靠它挑出用户实际用的那一条。
pub fn find_fwd_save_target(
    key: &str,
    title_hint: Option<&str>,
) -> Result<FwdSaveTarget, FwdTargetErr> {
    find_fwd_save_target_in(&load_all(), key, title_hint)
}

/// 同上，但条目表可注入（测试用）。
fn find_fwd_save_target_in(
    entries: &[ConnEntry],
    key: &str,
    title_hint: Option<&str>,
) -> Result<FwdSaveTarget, FwdTargetErr> {
    let mut custom: Vec<String> = Vec::new();
    let mut aliases: Vec<String> = Vec::new();
    for e in entries {
        if entry_session_key(e) != key {
            continue;
        }
        if e.custom {
            custom.push(e.name.clone());
        } else {
            aliases.push(e.name.clone());
        }
    }
    if let Some(name) = custom.first() {
        return Ok(FwdSaveTarget::Custom(name.clone()));
    }
    match aliases.len() {
        0 => Err(FwdTargetErr::NoEntry),
        1 => Ok(FwdSaveTarget::Alias(aliases[0].clone())),
        _ => {
            // 多个别名指向同一台机器：会话标题（= 条目名）能挑出是哪一条
            if let Some(t) = title_hint {
                if let Some(hit) = aliases.iter().find(|n| n.as_str() == t) {
                    return Ok(FwdSaveTarget::Alias(hit.clone()));
                }
            }
            Err(FwdTargetErr::Ambiguous(aliases))
        }
    }
}

/// 按目标写回转发规则（自建条目 / ssh 别名两种落点）。
pub fn save_inproc_forwards_to_target(target: &FwdSaveTarget, forwards: &str) -> bool {
    match target {
        FwdSaveTarget::Custom(name) => save_inproc_forwards(name, forwards),
        FwdSaveTarget::Alias(alias) => save_alias_forwards(alias, forwards),
    }
}

/// 写 ssh config 别名的进程内转发规则（`[ssh_alias_forwards]` 段）。
pub fn save_alias_forwards(alias: &str, forwards: &str) -> bool {
    save_alias_forwards_to(&ini_path(), alias, forwards)
}

fn save_alias_forwards_to(path: &std::path::Path, alias: &str, forwards: &str) -> bool {
    let mut kf = ini_load_from(path);
    let rules = forward::parse_rules(forwards);
    if rules.is_empty() {
        let _ = kf.remove_key(ALIAS_FWD_GROUP, alias);
    } else {
        kf.set_string(
            ALIAS_FWD_GROUP,
            alias,
            &forward::to_storage_value(&rules),
        );
    }
    ini_save_to(&kf, path)
}

/// 只改 forwardings 键（不动同组其他字段）：转发管理窗口或外部
/// 编辑器改规则后局部回写，避免整条覆盖把并发修改抹掉。
pub fn save_forwards(name: &str, forwards: &str) -> bool {
    save_forwards_to(&ini_path(), name, forwards)
}

/// [`save_forwards`] 的可注入路径版（测试不碰用户真实配置）。
pub fn save_forwards_to(path: &std::path::Path, name: &str, forwards: &str) -> bool {
    let mut kf = ini_load_from(path);
    let mut target = None;
    for g in kf.groups() {
        if kf.string(&g, "name").ok().as_deref() == Some(name) {
            target = Some(g.to_string());
            break;
        }
    }
    let Some(target) = target else { return false };
    let rules = forward::parse_rules(forwards);
    if rules.is_empty() {
        let _ = kf.remove_key(&target, "forwardings");
    } else {
        kf.set_string(&target, "forwardings", &forward::to_storage_value(&rules));
    }
    ini_save_to(&kf, path)
}

#[allow(clippy::too_many_arguments)]
fn save_entry_to(
    path: &std::path::Path,
    name: &str,
    host: &str,
    port: i32,
    user: Option<&str>,
    password: Option<&str>,
    proto: Option<&str>,
    sercfg: Option<&str>,
    forwards: Option<&str>,
) -> bool {
    let kf = ini_load_from(path);
    // 先找同名组
    let mut target: Option<String> = None;
    for g in kf.groups() {
        if let Ok(n) = kf.string(&g, "name") {
            if n == name {
                target = Some(g.to_string());
                break;
            }
        }
    }
    let target = target.unwrap_or_else(|| {
        // 新增组名 conn<N>：N = 现有最大序号 + 1
        let mut maxn: i64 = 0;
        for g in kf.groups() {
            if let Some(rest) = g.strip_prefix("conn") {
                let v = atoi_like(rest);
                if v > maxn {
                    maxn = v;
                }
            }
        }
        format!("conn{}", maxn + 1)
    });

    kf.set_string(&target, "name", name);
    kf.set_string(&target, "host", host);
    kf.set_uint64(&target, "port", if port > 0 { port as u64 } else { 22 });
    match user.filter(|u| !u.is_empty()) {
        Some(u) => kf.set_string(&target, "user", u),
        None => {
            let _ = kf.remove_key(&target, "user");
        }
    }
    match proto.filter(|p| !p.is_empty() && *p != "ssh") {
        Some(p) => kf.set_string(&target, "proto", p),
        None => {
            let _ = kf.remove_key(&target, "proto");
        }
    }
    match sercfg.filter(|c| !c.is_empty()) {
        Some(c) => kf.set_string(&target, "sercfg", c),
        None => {
            let _ = kf.remove_key(&target, "sercfg");
        }
    }
    // 转发规则：坏条目 parse_rules 已丢弃，落盘的是规整后的内部形
    let rules = forwards
        .map(|f| forward::parse_rules(f))
        .unwrap_or_default();
    if rules.is_empty() {
        let _ = kf.remove_key(&target, "forwardings");
    } else {
        kf.set_string(&target, "forwardings", &forward::to_storage_value(&rules));
    }
    // 密码：凭据库已解锁时只进 vault（加密），并把 ini 里的明文键删掉；
    // 未解锁（没建库 / 没解锁）则沿用旧的明文路径，保证功能不倒退。
    match password.filter(|p| !p.is_empty()) {
        Some(p) => {
            if crate::vault::is_unlocked() {
                let _ = crate::vault::put(name, p);
                let _ = kf.remove_key(&target, "password");
            } else {
                kf.set_string(&target, "password", p);
            }
        }
        None => {
            let _ = kf.remove_key(&target, "password");
        }
    }
    ini_save_to(&kf, path)
}

pub fn delete_entry(name: &str) -> bool {
    delete_entry_from(&ini_path(), name)
}

fn delete_entry_from(path: &std::path::Path, name: &str) -> bool {
    let kf = ini_load_from(path);
    let mut found = false;
    for g in kf.groups() {
        if let Ok(n) = kf.string(&g, "name") {
            if n == name {
                let _ = kf.remove_group(&g);
                found = true;
            }
        }
    }
    found && ini_save_to(&kf, path)
}

pub fn entry_addr(e: &ConnEntry) -> String {
    let mut s = String::new();
    if let Some(u) = &e.user {
        if !u.is_empty() {
            s.push_str(u);
            s.push('@');
        }
    }
    s.push_str(&e.host);
    if e.port > 0 && e.port != 22 {
        s.push_str(&format!(":{}", e.port));
    }
    s
}

/* ---------------- 2.3 会话库导入导出 ---------------- */

/// 导入 PuTTY 会话到 connections.ini。
/// 重名条目一律跳过——覆盖会把用户在 PuTTYPlus 里改过的配置弄丢，
/// 而"没导入"是用户自己能看见、能重来的。
pub fn import_putty_sessions(dir: &std::path::Path) -> Result<ImportReport, String> {
    let mut rep = ImportReport::default();
    let existing: std::collections::HashSet<String> =
        load_all().into_iter().map(|e| e.name).collect();

    let Ok(rd) = std::fs::read_dir(dir) else {
        return Err(trf!("读不到会话目录 {v0}", v0 = dir.display()));
    };
    let mut files: Vec<_> = rd
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .collect();
    files.sort_by_key(|e| e.file_name());

    for f in files {
        let enc = f.file_name().to_string_lossy().into_owned();
        let name = crate::config::session_filename_decode(&enc);
        if name.is_empty() || name.starts_with('.') {
            continue;
        }
        if existing.contains(&name) {
            rep.skipped.push(name);
            continue;
        }
        let mut o = crate::args::ConnOptions::default();
        let mut warnings = Vec::new();
        if !crate::config::load_putty_session_from_dir(&mut o, &name, dir, &mut warnings) {
            continue;
        }
        if o.host.is_empty() {
            continue;
        }
        // 串口条目：把 ConnOptions 的串口参数折回 sercfg 字符串存
        let sercfg = if o.proto == "serial" {
            Some(format!("{},{},{},{},{}",
                o.serial_baud, o.serial_data, o.serial_parity, o.serial_stop, o.serial_flow
            ))
        } else {
            None
        };
        let fwd = crate::backend::forward::to_storage_value(
            &o.forwards
                .iter()
                .flat_map(|f| crate::backend::forward::parse_rules(f))
                .collect::<Vec<_>>(),
        );
        if !save_entry(
            &name,
            &o.host,
            if o.port > 0 { o.port } else { 22 },
            Some(&o.user),
            None,
            Some(&o.proto),
            sercfg.as_deref(),
            Some(&fwd),
        ) {
            return Err(trf!("写入条目「{name}」失败", name = name));
        }
        rep.imported.push(name);
    }
    Ok(rep)
}

#[derive(Default)]
pub struct ImportReport {
    pub imported: Vec<String>,
    pub skipped: Vec<String>,
}

/// 导出选中条目为 PuTTY 会话文件。
pub fn export_putty_sessions(dir: &std::path::Path, names: &[String]) -> ExportReport {
    let mut rep = ExportReport::default();
    for e in load_all() {
        if !names.contains(&e.name) {
            continue;
        }
        let o = e.to_opts();
        // to_opts 保留的是启用的转发规则，停用项不进 argv/会话文件
        let o = crate::args::ConnOptions {
            forwards: e
                .forwards
                .iter()
                .filter(|r| r.enabled)
                .map(|r| format!("{}:{}", r.kind, r.spec))
                // 进程内映射也导出：PuTTY 的 PortForwardings 与它是同一个
                // 概念，导出会变成"由 ssh 承接"——但不能静默丢规则
                .chain(
                    e.inproc_forwards
                        .iter()
                        .filter(|r| r.enabled)
                        .map(|r| format!("{}:{}", r.kind, r.spec)),
                )
                .collect(),
            ..o
        };
        match crate::config::write_putty_session(dir, &e.name, &o) {
            Ok(skipped) => {
                rep.exported.push(e.name);
                rep.skipped_rules.extend(skipped);
            }
            Err(err) => rep.errors.push(format!("{}: {err}", e.name)),
        }
    }
    rep
}

#[derive(Default)]
pub struct ExportReport {
    pub exported: Vec<String>,
    /// 写了会话但没能表达的转发规则（停用项 / 带监听地址的 L/R）
    pub skipped_rules: Vec<String>,
    pub errors: Vec<String>,
}

/// 导入：选 PuTTY 会话目录。
///
/// GTK4 的 DialogExt 没有阻塞 run()，目录选择与后续导入都挂在
/// connect_response 上——和项目里其他对话框一个路子。
pub fn ui_import_putty(parent: Option<&gtk::Window>) {
    // ⚠️ 兜底到主窗口（阶段 67）：不给父窗口时也得挂一个
    let parent = parent_of(parent);
    let dlg = crate::windows::file_chooser_dialog(
        tr("选择 PuTTY 会话目录"),
        parent.as_ref(),
        gtk::FileChooserAction::SelectFolder,
    );
    dlg.add_button(tr("取消"), gtk::ResponseType::Cancel);
    dlg.add_button(tr("导入"), gtk::ResponseType::Accept);
    dlg.set_default_response(gtk::ResponseType::Accept);

    dlg.connect_response(move |dlg, resp| {
        let pwin = parent.clone();
        let dir = dlg.file().and_then(|f| f.path());
        dlg.destroy();
        if resp != gtk::ResponseType::Accept {
            return;
        }
        let Some(dir) = dir else { return };
        match import_putty_sessions(&dir) {
            Err(e) => crate::panel::dialogs::show_error(pwin.as_ref(), tr("导入失败"), &e),
            Ok(rep) => {
                let mut msg = trf!("已导入 {v0} 个会话。", v0 = rep.imported.len());
                if !rep.skipped.is_empty() {
                    msg.push_str(&trf!("\n跳过（重名已存在）{v0} 个：{v1}", v0 = rep.skipped.len(), v1 = rep.skipped.join(tr("、"))));
                }
                crate::panel::dialogs::info(pwin.as_ref(), tr("导入 PuTTY 会话"), &msg);
            }
        }
    });
    crate::panel::dialogs::style_dialog(dlg.upcast_ref::<gtk::Dialog>());
    dlg.present();
}

/// 导出：选 PuTTY 会话目录 + 选中要导出的条目。
pub fn ui_export_putty(parent: Option<&gtk::Window>, names: Vec<String>) {
    if names.is_empty() {
        crate::panel::dialogs::show_error(
            parent,
            tr("导出"),
            tr("没有可导出的连接（自定义连接为空）。"),
        );
        return;
    }
    // ⚠️ 兜底到主窗口（阶段 67）
    let parent = parent_of(parent);
    let dlg = crate::windows::file_chooser_dialog(
        tr("导出 PuTTY 会话到目录"),
        parent.as_ref(),
        gtk::FileChooserAction::SelectFolder,
    );
    dlg.add_button(tr("取消"), gtk::ResponseType::Cancel);
    dlg.add_button(tr("导出"), gtk::ResponseType::Accept);
    dlg.set_default_response(gtk::ResponseType::Accept);

    dlg.connect_response(move |dlg, resp| {
        let pwin = parent.clone();
        let dir = dlg.file().and_then(|f| f.path());
        dlg.destroy();
        if resp != gtk::ResponseType::Accept {
            return;
        }
        let Some(dir) = dir else { return };
        let rep = export_putty_sessions(&dir, &names);
        let mut msg = trf!("已导出 {v0} 个会话到 {v1}。", v0 = rep.exported.len(), v1 = dir.display());
        if !rep.skipped_rules.is_empty() {
            msg.push_str(&trf!("\n\n⚠ 有规则没能写进 PuTTY 会话（该格式表达不了），仍留在 PuTTYPlus 里：\n{v0}", v0 = rep.skipped_rules.join("\n")));
        }
        if !rep.errors.is_empty() {
            msg.push_str(&trf!("\n\n失败：\n{v0}", v0 = rep.errors.join("\n")));
            crate::panel::dialogs::show_error(pwin.as_ref(), tr("导出 PuTTY 会话"), &msg);
        } else {
            crate::panel::dialogs::info(pwin.as_ref(), tr("导出 PuTTY 会话"), &msg);
        }
    });
    crate::panel::dialogs::style_dialog(dlg.upcast_ref::<gtk::Dialog>());
    dlg.present();
}

/* ---------------- 自我 spawn（密码走环境变量） ---------------- */

/// 自 spawn 的**参数向量**（不含可执行文件路径自身）。
///
/// ⚠️ 单独抽出来是为了**能测**：串口曾经把设备路径传了两次（一次给
/// `-serial`、一次当位置参数）⇒ 位置参数那一轮撞上「host 已有值」
/// ⇒ 设备路径被当成**远程命令**发出去。
fn spawn_self_args(e: &ConnEntry) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    // 按协议派发子命令：串口要带 sercfg，Telnet 带 -telnet，
    // 否则新窗口里又是按 SSH 连（与 GUI 侧同一个坑）
    match e.proto.as_str() {
        "serial" => {
            // ⚠️ **-serial 是不带值的协议开关**（与 C 版一致）：设备路径走
            // **位置参数**——也就是下面统一 push 的那个 e.host。
            cmd_arg(&mut a, "-serial");
            if let Some(c) = e.sercfg.as_deref().filter(|c| !c.is_empty()) {
                cmd_arg(&mut a, "-sercfg");
                cmd_arg(&mut a, c);
            }
        }
        "telnet" => cmd_arg(&mut a, "-telnet"),
        _ => cmd_arg(&mut a, "-ssh"),
    }
    if let Some(u) = &e.user {
        if !u.is_empty() {
            cmd_arg(&mut a, "-l");
            cmd_arg(&mut a, u);
        }
    }
    if e.port > 0 {
        cmd_arg(&mut a, "-P");
        cmd_arg(&mut a, &e.port.to_string());
    }
    if !e.name.is_empty() {
        cmd_arg(&mut a, "-title");
        cmd_arg(&mut a, &e.name);
    }
    cmd_arg(&mut a, &e.host);
    a
}

/// 往参数向量里塞一个参数（`&str` 与 `String` 都能塞）。
fn cmd_arg(a: &mut Vec<String>, v: &str) {
    a.push(v.to_string());
}

/// 用选中条目派生新窗口。密码通过 PUTTYPLUS_PW_SESSION 传递，
/// 不进 argv（修复 C 版 ps 可见问题）。
fn spawn_self(e: &ConnEntry) -> Result<(), String> {
    // ⚠️ 跨平台版走 `platform::exe`（Linux 仍是 `/proc/self/exe`，
    // Windows 用 `current_exe()`）——**错误行为与改造前一致**（原样透传 io 错误）。
    let exe = crate::platform::exe::self_exe_path().map_err(|err| err.to_string())?;
    let mut cmd = std::process::Command::new(exe);
    for a in spawn_self_args(e) {
        cmd.arg(a);
    }
    if let Some(pw) = e.password.as_ref().filter(|p| !p.is_empty()) {
        cmd.env("PUTTYPLUS_PW_SESSION", pw.as_str());
    }
    // ⚠️ **标记「这是我们自己派生的窗口」**：用户在界面里点的
    //    「在新窗口打开」⇒ 这条连接该照常记进「最近连接」。
    //    不带这个标记的话，它会跟"堡垒机从外面拉起的窗口"一样，
    //    被当成"命令行启动"而不记录。
    cmd.env(crate::args::INTERNAL_SPAWN_ENV, "1");
    cmd.spawn().map(|_| ()).map_err(|err| err.to_string())
}

/// 从 PUTTYPLUS_PW_SESSION 兜底读取密码（由 spawn 出来的子进程用）。
pub fn password_from_env() -> Option<Zeroizing<String>> {
    std::env::var("PUTTYPLUS_PW_SESSION")
        .ok()
        .filter(|s| !s.is_empty())
        .map(Zeroizing::new)
}

/* ---------------- 对话框父窗口 ---------------- */

// 对话框从按钮、菜单、欢迎页到处都能拉起；统一挂到主窗口上，免得每个
// 调用点都穿一遍 parent（也避免 GTK 的"无 transient parent"警告）。
fn parent_of(given: Option<&gtk::Window>) -> Option<gtk::Window> {
    crate::windows::parent_of(given)
}

/* ---------------- 条目编辑对话框 ---------------- */

struct EntryForm {
    w_name: gtk::Entry,
    w_host: gtk::Entry,
    w_port: gtk::Entry,
    w_proto: gtk::ComboBoxText,
    serial: SerialFields,
}

/// 串口参数拆成五个独立控件（波特率/数据位/校验/停止位/流控）。
/// 持久化仍是 PuTTY 兼容的 `-sercfg` 字符串，UI 只负责拆装。
#[derive(Clone)]
struct SerialFields {
    baud: gtk::Entry,
    data: gtk::ComboBoxText,
    parity: gtk::ComboBoxText,
    stop: gtk::ComboBoxText,
    flow: gtk::ComboBoxText,
    box_: gtk::Box,
}

impl SerialFields {
    fn new() -> Self {
        let baud = gtk::Entry::new();
        baud.set_width_chars(8);
        baud.set_max_width_chars(12);
        baud.set_tooltip_text(Some(tr("常用：9600 / 57600 / 115200；可填任意正整数")));
        let data = gtk::ComboBoxText::new();
        for v in ["5", "6", "7", "8"] {
            data.append(Some(v), v);
        }
        data.set_tooltip_text(Some(tr("数据位：5–8")));
        let parity = gtk::ComboBoxText::new();
        parity.append(Some("n"), tr("无 (n)"));
        parity.append(Some("e"), tr("偶 (e)"));
        parity.append(Some("o"), tr("奇 (o)"));
        parity.set_tooltip_text(Some(tr("校验位：无 / 偶 / 奇")));
        let stop = gtk::ComboBoxText::new();
        stop.append(Some("1"), "1");
        stop.append(Some("2"), "2");
        stop.set_tooltip_text(Some(tr("停止位：1 / 2")));
        let flow = gtk::ComboBoxText::new();
        flow.append(Some("n"), tr("无 (n)"));
        flow.append(Some("x"), "XON/XOFF (x)");
        flow.append(Some("r"), "RTS/CTS (r)");
        flow.set_tooltip_text(Some(tr("流控：无 / 软件 / 硬件")));

        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 0);
        box_.append(&crate::panel::dialogs::form_pair(
            tr("波特率"),
            &baud,
            tr("数据位"),
            &data,
        ).0);
        box_.append(&crate::panel::dialogs::form_pair(
            tr("校验位"),
            &parity,
            tr("停止位"),
            &stop,
        ).0);
        box_.append(&crate::panel::dialogs::form_row(tr("流控"), &flow));

        let f = SerialFields {
            baud,
            data,
            parity,
            stop,
            flow,
            box_,
        };
        f.set("115200,8,n,1");
        f
    }

    /// 从 sercfg 字符串填控件（非法段按 PuTTY 语义回落默认）。
    fn set(&self, val: &str) {
        let c = crate::args::parse_sercfg(val);
        self.baud.set_text(&c.baud.to_string());
        self.data.set_active_id(Some(match c.data {
            5 => "5",
            6 => "6",
            7 => "7",
            _ => "8",
        }));
        self.parity.set_active_id(Some(match c.parity {
            'e' => "e",
            'o' => "o",
            _ => "n",
        }));
        self.stop.set_active_id(Some(if c.stop == 2 { "2" } else { "1" }));
        self.flow.set_active_id(Some(match c.flow {
            'x' => "x",
            'r' => "r",
            _ => "n",
        }));
    }

    /// 控件 → sercfg 字符串；流控为 n 时省略末段，保持旧存储格式。
    fn value(&self) -> String {
        let baud = atoi_like(&self.baud.text()).max(1);
        let data = self.data.active_id().unwrap_or_else(|| "8".into());
        let parity = self.parity.active_id().unwrap_or_else(|| "n".into());
        let stop = self.stop.active_id().unwrap_or_else(|| "1".into());
        let flow = self.flow.active_id().unwrap_or_else(|| "n".into());
        if flow == "n" {
            format!("{baud},{data},{parity},{stop}")
        } else {
            format!("{baud},{data},{parity},{stop},{flow}")
        }
    }
}

/// 新建/编辑对话框（异步）。回调参数：None=取消。
#[allow(clippy::type_complexity)]
/// 连接编辑对话框的产出。原先是 7 元组，再加转发规则就没法读了。
struct EntryDraft {
    name: String,
    host: String,
    port: i32,
    user: String,
    password: String,
    proto: String,
    sercfg: String,
    /// forwardings 存储值（内部形，`!` 标停用）
    forwards: String,
    /// inprocess_forwardings 存储值（进程内端口映射，格式同上）
    inproc_forwards: String,
    /// 会话默认设置（连接编辑器的「会话」页，阶段 69 之二）
    sess: SessionOpts,
    /// 认证方式（连接编辑器的「认证」页，阶段 70）
    auth_method: crate::args::AuthMethod,
    /// 私钥文件（阶段 70）；None = 不指定
    keyfile: Option<String>,
    /// 是否记住密码（阶段 70）：不记住 = 密码不落盘、也不进凭据库
    remember: bool,
}

/// 「认证」页读出来的内容（阶段 70）。
#[derive(Debug, Clone, Default, PartialEq)]
struct AuthDraft {
    user: String,
    password: String,
    auth_method: crate::args::AuthMethod,
    keyfile: Option<String>,
    /// 是否记住密码（不记住 = 密码不落盘，也不进凭据库）
    remember: bool,
}

/// 「认证」页的产出（阶段 70）。
struct AuthPage {
    /// 页面容器（放进页签）
    root: gtk::Box,
    /// 非 SSH 连接时的说明（协议切换时显隐）
    note: gtk::Label,
    /// 用户名
    w_user: gtk::Entry,
    /// 密码
    w_pass: gtk::Entry,
    /// 读当前值
    read: Rc<dyn Fn() -> AuthDraft>,
}

/// 连接编辑器的「认证」页（阶段 70，审计 P1-5）。
///
/// 三件事：
/// 1. **认证方式**：允许用哪些凭据（自动 / 只用 agent / 只用私钥文件 /
///    只用密码）——以前一个连接只能用"密码或密钥"两条路，没有"只准用
///    密码"这种说法（不想把本地身份出示给堡垒机网关时就需要它）；
/// 2. **私钥选择器**：列出 `~/.ssh` 里的私钥（私钥管理器列的也是这些），
///    也可以自己挑一个别的路径；
/// 3. **「记住密码」+ 状态行**：凭据库是什么状态就地说清楚。没启用/锁着
///    时这个勾**不可用**——勾了也只是明文写进 `connections.ini`，用户
///    以为"记住密码"就是加密保存，正好了。
fn auth_page(parent: Option<&gtk::Window>, init: Option<&ConnEntry>) -> AuthPage {
    use crate::args::AuthMethod;

    let root = crate::panel::dialogs::form_box();

    // 非 SSH 连接的说明（协议切到 telnet/串口时显出来）
    let note = gtk::Label::new(Some(tr("这些设置只对 SSH 连接有意义。")));
    note.add_css_class("dim-label");
    note.add_css_class("pp-hint");
    note.set_xalign(0.0);
    note.set_wrap(true);
    note.set_visible(false);
    root.append(&note);

    // ---- 认证方式 ----
    let w_method = gtk::ComboBoxText::new();
    for m in [
        AuthMethod::Auto,
        AuthMethod::Agent,
        AuthMethod::KeyFile,
        AuthMethod::Password,
    ] {
        w_method.append(Some(m.as_str()), m.label());
    }
    let cur_method = init.map(|e| e.auth_method).unwrap_or_default();
    w_method.set_active_id(Some(cur_method.as_str()));
    w_method.set_tooltip_text(Some(
        tr("决定这条连接允许用哪些凭据。\n\
         自动 = 按认证优先级依次尝试（与以前完全一样）。\n\
         只影响本程序自己的 SFTP 通道；终端里跑的是 ssh 客户端，\
         它按 ~/.ssh/config 与命令行来。"),
    ));

    // ---- 用户名 / 密码 ----
    let w_user = gtk::Entry::new();
    let w_pass = gtk::Entry::new();
    w_pass.set_visibility(false);
    w_pass.set_tooltip_text(Some(
        tr("可选。留空则连接时在终端里输入。\n\
         密码不会进命令行参数（ps 看不见），也不会明文进日志。"),
    ));
    if let Some(e) = init {
        if let Some(u) = &e.user {
            w_user.set_text(u);
        }
        if let Some(p) = &e.password {
            // 密码在条目里可能是凭据库里的（读出来就是明文）
            w_pass.set_text(p);
        }
    }

    // ---- 私钥文件 ----
    let cur_key = init
        .and_then(|e| e.keyfile.clone())
        .filter(|k| !k.is_empty());
    let w_key = gtk::ComboBoxText::new();
    w_key.append(Some(NO_KEY_ID), tr("（不指定私钥）"));
    // 下拉框里有哪些 id：自己记一份（gtk4 的 StringList 不是 TreeModel，
    // 拿不到 model 就没法反查）
    let key_ids = std::rc::Rc::new(std::cell::RefCell::new(vec![NO_KEY_ID.to_string()]));
    // ~/.ssh 里的私钥（私钥管理器列的也是这些）
    if let Some(dir) = crate::keymgr::ssh_dir() {
        for k in crate::keymgr::list_dir(&dir) {
            let path = k.path.to_string_lossy().into_owned();
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.clone());
            w_key.append(Some(path.as_str()), name.as_str());
            key_ids.borrow_mut().push(path);
        }
    }
    // 连接里存的私钥可能不在 ~/.ssh 里（用户自己挑的路径）⇒ 补进列表
    if let Some(k) = cur_key.as_deref() {
        if !key_ids.borrow().iter().any(|x| x == k) {
            let name = std::path::Path::new(k)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| k.to_string());
            w_key.append(Some(k), name.as_str());
            key_ids.borrow_mut().push(k.to_string());
        }
    }
    w_key.set_active_id(Some(cur_key.as_deref().unwrap_or(NO_KEY_ID)));
    w_key.set_tooltip_text(Some(
        tr("这条连接用的私钥文件（列表是 ~/.ssh 里的私钥，私钥管理器列的也是这些）。\n\
         留空 = 不指定；选了它，认证方式用「只用指定的私钥文件」时才生效。"),
    ));

    // 「浏览…」：挑一个别的私钥
    let w_key_browse = gtk::Button::with_label(tr("浏览…"));
    w_key_browse.set_tooltip_text(Some(tr("挑一个不在 ~/.ssh 里的私钥文件。")));
    {
        let w_key = w_key.clone();
        let key_ids = key_ids.clone();
        w_key_browse.connect_clicked(move |b| {
            let parent = b
                .root()
                .and_then(|r| r.downcast::<gtk::Window>().ok());
            let dlg = crate::windows::file_chooser_dialog(
                tr("选择私钥文件"),
                parent.as_ref(),
                gtk::FileChooserAction::Open,
            );
            dlg.add_button(tr("取消"), gtk::ResponseType::Cancel);
            dlg.add_button(tr("选定"), gtk::ResponseType::Accept);
            dlg.set_default_response(gtk::ResponseType::Accept);
            let w_key = w_key.clone();
            let key_ids = key_ids.clone();
            dlg.connect_response(move |dlg, resp| {
                let picked = if resp == gtk::ResponseType::Accept {
                    dlg.file().and_then(|f| f.path())
                } else {
                    None
                };
                dlg.destroy();
                let Some(p) = picked else { return };
                let p = p.to_string_lossy().into_owned();
                if !key_ids.borrow().iter().any(|x| *x == p) {
                    let name = std::path::Path::new(&p)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| p.clone());
                    w_key.append(Some(p.as_str()), name.as_str());
                    key_ids.borrow_mut().push(p.clone());
                }
                w_key.set_active_id(Some(p.as_str()));
            });
            dlg.present();
        });
    }
    let key_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    w_key.set_hexpand(true);
    key_row.append(&w_key);
    key_row.append(&w_key_browse);

    // ---- 记住密码 + 凭据库状态 ----
    //
    // ⚠️ 阶段 79：这块以前是**自己写的**勾选框 + 状态文案，
    // DB 密码提示那边又写了一份 ⇒ 同一件事两处说法、还各自漂移。
    // 现在三处共用 `vaultui::remember_row`（未启用 ⇒ 不可勾 + 说明；
    // 已锁定 ⇒ 给「解锁」入口）。
    // ⚠️ `None` = 不按会话判定：这里编辑的是**要保存的连接条目**
    //    （堡垒机场景压根不保存连接 ⇒ 这个对话框在那种场景下用不上）。
    let (w_remember, status_box) =
        crate::vaultui::remember_row(parent, !w_pass.text().is_empty(), None);
    // 勾的状态跟着密码框走：填了密码就是"要记住"
    {
        let w_remember = w_remember.clone();
        w_pass.connect_changed(move |e| {
            // ⚠️ 凭据库不可用时**别勾**：灰着的勾还打着勾，
            // 看着像"已经记住了"，其实密码压根没存。
            if w_remember.is_sensitive() {
                w_remember.set_active(!e.text().is_empty());
            }
        });
    }

    root.append(&crate::panel::dialogs::form_section(tr("凭据")));
    root.append(&crate::panel::dialogs::form_row(tr("认证方式"), &w_method));
    root.append(&crate::panel::dialogs::form_row(tr("用户名"), &w_user));
    root.append(&crate::panel::dialogs::form_row(tr("密码（可选）"), &w_pass));
    root.append(&crate::panel::dialogs::form_row(tr("私钥文件"), &key_row));
    root.append(&w_remember);
    root.append(&status_box);

    let tip = gtk::Label::new(Some(
        tr("「认证方式」决定这条连接允许用哪些凭据：只用密码就不会把本地身份\
         出示给堡垒机网关；只用私钥文件则不会去翻 ssh-agent。"),
    ));
    tip.add_css_class("dim-label");
    tip.add_css_class("pp-hint");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    root.append(&tip);

    let r_user = w_user.clone();
    let r_pass = w_pass.clone();
    let read: Rc<dyn Fn() -> AuthDraft> = Rc::new(move || {
        let keyfile = w_key
            .active_id()
            .filter(|k| *k != NO_KEY_ID)
            .map(|k| k.to_string());
        AuthDraft {
            user: r_user.text().to_string(),
            password: r_pass.text().to_string(),
            auth_method: w_method
                .active_id()
                .as_deref()
                .and_then(AuthMethod::parse)
                .unwrap_or_default(),
            keyfile,
            remember: w_remember.is_active(),
        }
    });

    AuthPage {
        root,
        note,
        w_user,
        w_pass,
        read,
    }
}

/// 「不指定私钥」在私钥下拉框里的 id（空串不能当 id 用）。
const NO_KEY_ID: &str = "-";

/// 「转发」页的产出（阶段 71）。
struct FwdPage {
    /// 页面容器
    root: gtk::Box,
    /// 结构化表
    table: Rc<crate::fwdtable::FwdTable>,
    /// 原始规则域（折叠区里）：终端 / 进程内
    raw_term: gtk::TextView,
    raw_inproc: gtk::TextView,
    /// 上次同步进原始域的内容（判断用户有没有手改过）
    synced_term: Rc<std::cell::RefCell<String>>,
    synced_inproc: Rc<std::cell::RefCell<String>>,
}

/// 连接编辑器的「转发」页（阶段 71，审计 P1-3）。
///
/// 以前是两个原始规则域（终端转发 / 进程内端口映射），长得一模一样，用户
/// 分不清哪条走哪套机制；现在是一张结构化表（类型/监听/目标/机制/启用），
/// 两套机制在**同一张表**里。
///
/// ⚠️ 原始规则域没删，挪到表下面的折叠区里：PuTTY 会话里拷来的规则还是
/// 按那套文本格式粘贴进来方便。**原始域被手改过就以它为准**（粘贴导入
/// 的路径），否则以表为准。
fn fwd_page(init: Option<&ConnEntry>) -> FwdPage {
    let (term_val, inproc_val) = match init {
        Some(e) => (
            forward::to_storage_value(&e.forwards),
            forward::to_storage_value(&e.inproc_forwards),
        ),
        None => (String::new(), String::new()),
    };
    let rows = crate::fwdtable::rows_from_storage(&term_val, &inproc_val);
    let table = Rc::new(crate::fwdtable::FwdTable::new(rows));

    let root = crate::panel::dialogs::form_box();

    // ---- 工具栏 ----
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let b_add = gtk::Button::with_label(tr("添加…"));
    b_add.set_tooltip_text(Some(tr("加一条转发规则（类型与机制在弹出的对话框里选）。")));
    let b_edit = gtk::Button::with_label(tr("编辑…"));
    b_edit.set_tooltip_text(Some(tr("改选中那条的类型与机制（双击表格里的行也行）。")));
    let b_del = gtk::Button::with_label(tr("删除"));
    b_del.set_tooltip_text(Some(tr("删掉选中那条。")));
    bar.append(&b_add);
    bar.append(&b_edit);
    bar.append(&b_del);

    let hint = gtk::Label::new(Some(
        tr("「监听」「目标」两列双击就能改；「启用」点一下就切换。\n\
         终端 = 交给 spawn 出去的 ssh -L/-R/-D；进程内 = 跑在应用自己那条\
         SSH 连接上（终端标签页关了也照样在）。"),
    ));
    hint.add_css_class("dim-label");
    hint.add_css_class("pp-hint");
    hint.set_xalign(0.0);
    hint.set_wrap(true);

    root.append(&bar);
    root.append(&table.widget);
    root.append(&hint);

    // ---- 原始规则域（折叠区）----
    let raw_term = gtk::TextView::new();
    raw_term.set_monospace(true);
    raw_term.set_tooltip_text(Some(
        tr("一行一条：L:[监听地址:]端口:目标:端口（本地）/ R:监听端口:目标:端口（远程）/ \
         D:端口（SOCKS）。行首 ! 表示停用该条规则。\n\
         这里改了会覆盖上表（粘贴 PuTTY 会话里的规则走这条路）。"),
    ));
    let raw_inproc = gtk::TextView::new();
    raw_inproc.set_monospace(true);
    raw_inproc.set_tooltip_text(Some(
        tr("跑在应用自己那条 SSH 连接上（不新建 SSH 连接、终端标签页关了也照样在）。\n\
         堡垒机场景用它把内网服务映射到本机。格式与上面相同，行首 ! 同样是停用。"),
    ));
    raw_term.buffer().set_text(&term_val);
    raw_inproc.buffer().set_text(&inproc_val);
    let synced_term = Rc::new(std::cell::RefCell::new(term_val));
    let synced_inproc = Rc::new(std::cell::RefCell::new(inproc_val));

    let expander = gtk::Expander::new(Some(tr("高级：原始规则（粘贴导入用）")));
    let exp_box = gtk::Box::new(gtk::Orientation::Vertical, 8);
    exp_box.append(&crate::panel::dialogs::form_section(tr("终端转发")));
    exp_box.append(&scroll_of(&raw_term));
    exp_box.append(&crate::panel::dialogs::form_section(tr("进程内端口映射")));
    exp_box.append(&scroll_of(&raw_inproc));
    expander.set_child(Some(&exp_box));
    root.append(&expander);

    // ---- 按钮 ----
    {
        let table = table.clone();
        b_add.connect_clicked(move |b| {
            let parent = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let row = crate::fwdtable::FwdRow::new('L', "", "", false);
            let t = table.clone();
            crate::fwdtable::edit_dialog(parent.as_ref(), &row, move |r| {
                if let Some(r) = r {
                    t.add(r);
                }
            });
        });
    }
    {
        let table = table.clone();
        b_edit.connect_clicked(move |b| {
            let Some(idx) = table.selected() else { return };
            let Some(row) = table.rows().get(idx).cloned() else {
                return;
            };
            let parent = b.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            let t = table.clone();
            crate::fwdtable::edit_dialog(parent.as_ref(), &row, move |r| {
                if let Some(r) = r {
                    t.update(idx, &r);
                }
            });
        });
    }
    {
        let table = table.clone();
        b_del.connect_clicked(move |_| {
            if let Some(idx) = table.selected() {
                table.remove(idx);
            }
        });
    }
    // 双击一行 = 改类型与机制
    {
        let table = table.clone();
        let view = table.view().clone();
        // 父窗口先取出来（闭包里再 borrow view 会和 move 打架）
        let parent = view.root().and_then(|r| r.downcast::<gtk::Window>().ok());
        let view2 = view.clone();
        view2.connect_row_activated(move |_v, path, _col| {
            let Some(idx) = path.indices().first().copied() else {
                return;
            };
            let Some(row) = table.rows().get(idx as usize).cloned() else {
                return;
            };
            let t = table.clone();
            let p = parent.clone();
            crate::fwdtable::edit_dialog(p.as_ref(), &row, move |r| {
                if let Some(r) = r {
                    t.update(idx as usize, &r);
                }
            });
        });
    }

    FwdPage {
        root,
        table,
        raw_term,
        raw_inproc,
        synced_term,
        synced_inproc,
    }
}

/// TextView 的全文。
fn text_of(tv: &gtk::TextView) -> String {
    let buf = tv.buffer();
    buf.text(&buf.start_iter(), &buf.end_iter(), false)
        .to_string()
}

/// 给一个 TextView 套滚动窗口（规则多时要能滚）。
fn scroll_of(tv: &gtk::TextView) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_min_content_height(76);
    sw.set_child(Some(tv));
    sw.set_hexpand(true);
    sw
}

/// 内置的保活间隔（秒）：与 `args::ConnOptions::default()` 一致。
const DEFAULT_KEEPALIVE_SEC: u32 = 30;

/// 连接编辑器的「会话」页（阶段 69 之二）。
///
/// 这页存的是**这条连接的默认值**（写进 connections.ini 的条目段）；
/// 运行时「会话选项…」对话框改的是**单个会话的临时覆盖**
/// （写进 settings.ini 的会话段）——后者优先。
///
/// ⚠️ **等于默认值的不落盘**：保活 30 秒 / 密码优先 / 响铃开，这些跟
/// 内置默认一样，存进去只是多一把没用的键；缺键 = 用内置默认，
/// 读回来的效果完全一致（`save_session_opts` 里 `None` 会把键删掉，
/// 所以从"密钥优先"改回"密码优先"会顺手清干净）。
///
/// 返回（页面容器, 读当前值的回调）。回调读**控件里的值**而不是控件本身：
/// 保存发生在 response 回调里，那时控件已经填好了。
fn session_page(init: Option<&ConnEntry>) -> (gtk::Box, Rc<dyn Fn() -> SessionOpts>) {
    let cur = init.map(|e| e.sess.clone()).unwrap_or_default();
    let root = crate::panel::dialogs::form_box();

    // 保活（秒）：0 = 关闭保活
    let w_keep = gtk::SpinButton::with_range(0.0, 600.0, 1.0);
    w_keep.set_digits(0);
    w_keep.set_value(f64::from(cur.keepalive_sec.unwrap_or(DEFAULT_KEEPALIVE_SEC)));
    w_keep.set_tooltip_text(Some(
        tr("无服务器数据多少秒后发一次 SSH 保活，连续 3 次无应答即断开。\n\
         0 = 关闭保活（完全依赖 TCP 超时）。"),
    ));

    // 终端类型（TERM）：自由填写，常用值写在提示里
    let w_term = gtk::Entry::new();
    w_term.set_text(cur.term_type.as_deref().unwrap_or(""));
    w_term.set_placeholder_text(Some(tr("留空 = xterm-256color")));
    w_term.set_tooltip_text(Some(
        tr("决定远程 pty 的 TERM。常用：xterm-256color / xterm / screen-256color / vt100 / linux。\n\
         改完要重新连接才生效（子进程环境在启动时定下）。"),
    ));

    // 认证优先级
    let w_auth = gtk::ComboBoxText::new();
    w_auth.append(Some("password-first"), tr("密码优先（默认）"));
    w_auth.append(Some("publickey-first"), tr("密钥优先"));
    w_auth.set_active_id(Some(cur.auth_order.unwrap_or_default().as_str()));
    w_auth.set_tooltip_text(Some(
        tr("进程内 SFTP 链路的认证顺序。\n\
         密码优先：少一次无谓的公钥尝试，也避免把本地身份出示给堡垒机网关。"),
    ));

    // 终端字号：留空 = 跟随首选项中心的全局字号
    let w_font = gtk::Entry::new();
    w_font.set_text(&cur.font_size.map(|v| format!("{v:.1}")).unwrap_or_default());
    w_font.set_placeholder_text(Some(tr("留空 = 跟随首选项")));
    w_font.set_tooltip_text(Some(
        tr("终端字号。留空则用首选项中心的全局字号（默认 10.5）。"),
    ));

    // 响铃：默认开
    let w_bell = gtk::CheckButton::with_label(tr("终端响铃"));
    w_bell.set_active(cur.bell.unwrap_or(true));

    root.append(&crate::panel::dialogs::form_section(tr("连接")));
    root.append(&crate::panel::dialogs::form_row(tr("保活（秒）"), &w_keep));
    root.append(&crate::panel::dialogs::form_row(
        tr("终端类型（TERM）"),
        &w_term,
    ));
    root.append(&crate::panel::dialogs::form_row(tr("认证优先级"), &w_auth));

    root.append(&crate::panel::dialogs::form_section(tr("终端")));
    root.append(&crate::panel::dialogs::form_row(tr("终端字号"), &w_font));
    root.append(&crate::panel::dialogs::form_row(tr("响铃"), &w_bell));

    let tip = gtk::Label::new(Some(
        tr("这里是这条连接的默认值，新开的会话都按它来。\n\
         保活、终端类型、认证顺序只对 SSH 连接有意义。\n\
         已经跑着的会话要单独改，用标签上的「会话选项…」——那是临时覆盖，优先于这里。"),
    ));
    tip.add_css_class("dim-label");
    tip.add_css_class("pp-hint");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    root.append(&tip);

    let read: Rc<dyn Fn() -> SessionOpts> = Rc::new(move || {
        let keep = w_keep.value().round() as i64;
        let keepalive_sec = if keep == i64::from(DEFAULT_KEEPALIVE_SEC) {
            None
        } else {
            Some(keep.clamp(0, 600) as u32)
        };
        let term_type = w_term.text().trim().to_string();
        let term_type = if term_type.is_empty() {
            None
        } else {
            Some(term_type)
        };
        let auth_order = w_auth
            .active_id()
            .as_deref()
            .and_then(crate::args::AuthOrder::parse);
        let auth_order = match auth_order {
            Some(crate::args::AuthOrder::PasswordFirst) => None,
            other => other,
        };
        let font_txt = w_font.text().trim().to_string();
        let font_size = if font_txt.is_empty() {
            None
        } else {
            // 非法输入 / 超出 6..=48 一律当作"没填"，不让一次手滑把字号写坏
            font_txt
                .parse::<f64>()
                .ok()
                .filter(|v| (6.0..=48.0).contains(v))
                .map(|v| (v * 10.0).round() / 10.0)
        };
        // 响铃默认开 ⇒ 与默认一样就不落盘
        let bell = if w_bell.is_active() { None } else { Some(false) };
        SessionOpts {
            keepalive_sec,
            term_type,
            auth_order,
            font_size,
            bell,
        }
    });

    (root, read)
}

/// 新建/编辑连接对话框的模式（阶段 73b，审计 P1-11）。
///
/// 以前「新建」是**四个**按钮：取消 / 仅连接 / 保存 / 保存并连接。
/// 用户分不清「是否保存」「是否连接」的区别，回车默认还落在「保存并连接」
/// 上——按错了就直接把连接存了。现在收敛成**一个主操作 + 一个勾选**：
///
/// - [`NewConnect`]：主操作「连接」，旁边一个「保存到连接列表」勾选
///   （勾上 = 保存并连接，不勾 = 仅连接）；
/// - [`Edit`]：主操作「保存」（改的是已有条目，没得连）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DlgMode {
    /// 新建：主操作「连接」+「保存到连接列表」勾选
    NewConnect,
    /// 编辑已有条目：主操作「保存」
    Edit,
}

fn entry_edit_dialog<F: FnOnce(Option<(EntryDraft, bool, bool)>) + 'static>(
    parent: Option<&gtk::Window>,
    init: Option<&ConnEntry>,
    mode: DlgMode,
    done: F,
) {
    let parent = parent_of(parent);
    // 动作按钮收敛成「取消」+ 一个主操作（阶段 73b）
    let buttons: Vec<(&str, gtk::ResponseType)> = match mode {
        DlgMode::NewConnect => vec![
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("连接"), gtk::ResponseType::Ok),
        ],
        DlgMode::Edit => vec![
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("保存"), gtk::ResponseType::Ok),
        ],
    };
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        match mode {
            DlgMode::NewConnect => tr("新建连接"),
            DlgMode::Edit => tr("编辑连接"),
        },
        parent.as_ref(),
        &buttons,
    );
    dlg.set_default_response(gtk::ResponseType::Ok);
    dlg.set_default_size(460, -1);

    let root = crate::panel::dialogs::form_box();

    let w_name = gtk::Entry::new();
    let w_host = gtk::Entry::new();
    let w_port = gtk::Entry::new();
    w_port.set_text("22");
    let w_proto = gtk::ComboBoxText::new();
    for (name, id) in [("SSH", "ssh"), ("Telnet", "telnet"), (tr("串口"), "serial")] {
        w_proto.append(Some(id), name);
    }
    let proto_default = init
        .map(|e| e.proto.clone())
        .unwrap_or_else(|| "ssh".to_string());
    w_proto.set_active_id(Some(proto_default.as_str()));

    // 串口参数拆开填，避免让人猜 "115200,8,n,1" 的逗号含义
    let serial = SerialFields::new();

    // 转发规则：多行文本，一行一条（内部形）。从 PuTTY 会话文件复制
    // PortForwardings 后经 from_putty_value 转成内部形即可粘贴。
    // ---- 表单版式：标签在上、控件在下；按协议整行显隐 ----
    root.append(&crate::panel::dialogs::form_section(tr("基本信息")));
    root.append(&crate::panel::dialogs::form_row(tr("名称"), &w_name));
    // 协议 + 端口并排：表单里最占高度的就是"一行一个字段"
    let (pair_port, _proto_row, port_row) =
        crate::panel::dialogs::form_pair(tr("协议"), &w_proto, tr("端口"), &w_port);
    root.append(&pair_port);
    let (row_host, l_host) = crate::panel::dialogs::form_row_with(tr("主机"), &w_host);
    root.append(&row_host);

    // ⚠️ 用户名/密码**不在这一页**了（阶段 70）：它们跟认证方式、私钥、
    // 「记住密码」是一组，搬进「认证」页（那一页整体按协议显隐）。

    let sec_serial = crate::panel::dialogs::form_section(tr("串口参数"));
    let serial_box = serial.box_.clone();
    root.append(&sec_serial);
    root.append(&serial_box);

    // ⚠️ 端口转发**不在这一页**了（阶段 71）：两段原始规则域（终端转发 /
    // 进程内端口映射）并成一张结构化表，放在「转发」页；原始规则域降级成
    // 那里面的折叠区（留着粘贴导入用）。

    if let Some(e) = init {
        w_name.set_text(&e.name);
        w_host.set_text(&e.host);
        if e.port > 0 {
            w_port.set_text(&e.port.to_string());
        }
        if let Some(c) = &e.sercfg {
            serial.set(c);
        }
    }

    // 「认证」页（阶段 70）：认证方式 + 私钥 + 密码 + 「记住密码」。
    let auth = auth_page(Some(dlg.upcast_ref::<gtk::Window>()), init);
    // 「会话」页：这条连接的默认设置（阶段 69 之二）。
    // 常规页已经很长了 ⇒ 用页签分开，别再往下堆。
    let (sess_page, read_sess) = session_page(init);
    let nb = gtk::Notebook::new();
    nb.set_vexpand(true);
    // 「转发」页（阶段 71）：结构化表 + 原始规则折叠区
    let fwd = fwd_page(init);
    nb.append_page(&root, Some(&gtk::Label::new(Some(tr("常规")))));
    nb.append_page(&auth.root, Some(&gtk::Label::new(Some(tr("认证")))));
    nb.append_page(&fwd.root, Some(&gtk::Label::new(Some(tr("转发")))));
    nb.append_page(&sess_page, Some(&gtk::Label::new(Some(tr("会话")))));

    // 显隐：串口隐藏端口/转发，显示「设备路径 + 串口参数」；
    // 非 SSH 时整页「认证」置灰（认证方式/私钥只对 SSH 有意义）
    let apply = {
        let l_host = l_host.clone();
        let port_row = port_row.clone();
        let serial_box = serial_box.clone();
        let sec_serial = sec_serial.clone();
        let auth_root = auth.root.clone();
        let auth_note = auth.note.clone();
        std::rc::Rc::new(move |proto: &str| {
            let serial = proto == "serial";
            let ssh = proto == "ssh";
            l_host.set_text(if serial { tr("设备路径") } else { tr("主机") });
            // 只藏"端口"这一半：协议对串口同样有意义
            port_row.set_visible(!serial);
            serial_box.set_visible(serial);
            sec_serial.set_visible(serial);
            // 「认证」页整体置灰 + 说明（不删页签：删了控件就没了）
            auth_root.set_sensitive(ssh);
            auth_note.set_visible(!ssh);
        })
    };
    apply(proto_default.as_str());
    {
        let apply = apply.clone();
        w_proto.connect_changed(move |cb| {
            let proto = cb
                .active_id()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "ssh".into());
            apply(proto.as_str());
        });
    }

    // ---- 版式：长表单套滚动容器 + 限高（阶段 73b，审计 P1-11）----
    //
    // 以前四个页签堆起来自然高度 810px，1366×768 的笔记本上**底部按钮区
    // 直接出屏**。现在把页签放进滚动容器并限高：内容再长也只占这么多，
    // 多出来的部分滚动看。
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    // ⚠️ **限高按屏幕高度自适应**（阶段 91，用户 2026-11-01 报
    // 「这个对话框可以长一点，把内容显示全，不要用滚动条」）：
    // 以前固定 405px ⇒「认证」页最后那段说明（凭据库状态）被切掉半截，
    // 而且**多出来一截滚动条**。现在：屏幕够高就一次显示全（不放滚动条），
    // 屏幕矮的笔记本才退回滚动。
    let screen_h = gtk::gdk::Display::default()
        .and_then(|d| {
            let mons = d.monitors();
            let first = mons.item(0)?;
            first.downcast::<gtk::gdk::Monitor>().ok()
        })
        .map(|m| m.geometry().height())
        .unwrap_or(1080);
    let max_h = (screen_h as i32 - 320).clamp(405, 640);
    sw.set_max_content_height(max_h);
    sw.set_propagate_natural_height(true);
    // ⚠️ **最小高度给"最高页签的自然高度 + 余量"**（阶段 91，用户
    // 2026-11-01 报「这个对话框可以长一点，把内容显示全不要用滚动条」）：
    //   · 以前固定限高 405px ⇒「认证」页最后那段说明（凭据库状态）被切掉
    //     半截，还多出一截滚动条；
    //   · **只给自然高度也不行**：内容**刚好等于**视口时 GTK 会因为取整
    //     仍挂出一根空滚动条 ⇒ 用户看到"明明显示全了却还有滚动条"；
    //   · **构建期量到的自然高度偏小**（字体是加载完才量的，实测差 20px
    //     左右）⇒ 余量给 64px，宁可多留一点空白，也不要多一根滚动条。
    let (_, tallest, _, _) = nb.measure(gtk::Orientation::Vertical, -1);
    sw.set_min_content_height((tallest + 64).min(max_h));
    sw.set_vexpand(true);
    sw.set_child(Some(&nb));

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 8);
    outer.append(&sw);

    // 「保存到连接列表」勾选：放在动作区正上方 ⇒ 一直看得见，
    // 也就不用四个按钮去表达「保存 / 仅连接 / 保存并连接」三种组合。
    //
    // ⚠️ 默认**勾上**：老对话框回车默认落在「保存并连接」上，默认行为不变。
    let save_flag = std::rc::Rc::new(std::cell::Cell::new(true));
    if mode == DlgMode::NewConnect {
        let save_to_list = gtk::CheckButton::with_label(tr("保存到连接列表"));
        save_to_list.set_active(true);
        save_to_list.set_tooltip_text(Some(
            tr("勾上：连接的同时把这条连接写进连接列表（侧栏里能找到）。\n\
             不勾：只连这一次，不留痕。"),
        ));
        {
            let save_flag = save_flag.clone();
            save_to_list.connect_toggled(move |b| save_flag.set(b.is_active()));
        }
        outer.append(&save_to_list);
    } else {
        save_flag.set(true);
    }

    dlg.content_area().append(&outer);
    crate::panel::dialogs::style_dialog(dlg.upcast_ref::<gtk::Dialog>());
    dlg.present();
    let form = EntryForm {
        w_name,
        w_host,
        w_port,
        w_proto,
        serial,
    };
    // 「认证」页的读值回调（阶段 70）
    let read_auth = auth.read.clone();
    // 转发页的句柄（响应时读表与原始规则域）
    let fwd_table = fwd.table.clone();
    let fwd_raw_term = fwd.raw_term.clone();
    let fwd_raw_inproc = fwd.raw_inproc.clone();
    let fwd_synced_term = fwd.synced_term.clone();
    let fwd_synced_inproc = fwd.synced_inproc.clone();
    let done = std::cell::RefCell::new(Some(done));
    dlg.connect_response(move |dlg, resp| {
        // 动作收敛成「取消」+ 一个主操作（阶段 73b）
        let ours = resp == gtk::ResponseType::Ok;
        if !ours {
            dlg.destroy();
            if let Some(f) = done.borrow_mut().take() {
                f(None);
            }
            return;
        }
        // 「连接」= 连；要不要落盘看「保存到连接列表」勾选（阶段 73b）
        let connect = mode == DlgMode::NewConnect;
        let save = save_flag.get();
        let n = form.w_name.text().to_string();
        let h = form.w_host.text().to_string();
        let serial = form.w_proto.active_id().as_deref() == Some("serial");
        let missing = if save {
            n.is_empty() || h.is_empty()
        } else {
            h.is_empty()
        };
        if missing {
            let msg = match (save, serial) {
                (true, true) => tr("名称和设备路径都不能为空"),
                (true, false) => tr("名称和主机都不能为空"),
                (false, true) => tr("设备路径不能为空"),
                (false, false) => tr("主机不能为空"),
            };
            let d = gtk::AlertDialog::builder().message(msg).modal(true).build();
            d.show(Some(dlg));
            return; // 保持对话框打开
        }
        let port = atoi_like(&form.w_port.text()) as i32;
        // 用户名/密码/认证方式/私钥：都在「认证」页上（阶段 70）
        let ad = read_auth();
        let user = ad.user;
        let pass = ad.password;
        let proto = form.w_proto.active_id().unwrap_or_else(|| "ssh".into()).to_string();
        let sercfg = if proto == "serial" {
            form.serial.value()
        } else {
            String::new()
        };
        // 转发（阶段 71）：**结构化表是主**；原始规则域被手改过就以它为准
        // （粘贴 PuTTY 会话里的规则走这条路）
        let (mut term_rows, mut inproc_rows) = crate::fwdtable::rows_split("", "");
        for r in fwd_table.rows() {
            if r.in_process {
                inproc_rows.push(r);
            } else {
                term_rows.push(r);
            }
        }
        let raw_term = text_of(&fwd_raw_term);
        if raw_term != *fwd_synced_term.borrow() {
            term_rows = crate::fwdtable::rows_from_storage(&raw_term, "");
        }
        let raw_inproc = text_of(&fwd_raw_inproc);
        if raw_inproc != *fwd_synced_inproc.borrow() {
            inproc_rows = crate::fwdtable::rows_from_storage("", &raw_inproc);
        }
        let (forwards, inproc_forwards) =
            crate::fwdtable::rows_to_storage(&[term_rows, inproc_rows].concat());
        dlg.destroy();
        if let Some(f) = done.borrow_mut().take() {
            f(Some((
                EntryDraft {
                    name: n,
                    host: h,
                    port,
                    user,
                    password: pass,
                    proto,
                    sercfg,
                    forwards,
                    inproc_forwards,
                    // 会话页的值（阶段 69 之二）
                    sess: read_sess(),
                    // 认证页的值（阶段 70）
                    auth_method: ad.auth_method,
                    keyfile: ad.keyfile,
                    remember: ad.remember,
                },
                save,
                connect,
            )));
        }
    });
}

/* ---------------- 手动连接快捷对话框（不保存） ---------------- */

/* ---------------- 管理器主对话框 ---------------- */


/// 「连接」回调：拿到参数后由调用方决定怎么开（当前窗口开标签 / 新窗口）。
type ConnectCb = std::rc::Rc<dyn Fn(ConnOptions)>;
/// 打开某个数据库连接（阶段 13）：`(会话键, 连接名)`。
///
/// 数据库通道复用 SSH 连接，所以"打开数据库连接"要先有那条 SSH 会话——
/// 由调用方（tabs.rs）负责：先把会话连上，连上后再开工作台。
type OpenDbCb = std::rc::Rc<dyn Fn(&str, &str)>;

/// 在侧边栏右键里"在新窗口打开"时用。
pub fn spawn_entry(e: &ConnEntry) -> Result<(), String> {
    spawn_self(e)
}

/// 侧边栏右键「编辑…」：改完落盘并回调（让侧边栏重读）。
pub fn ui_edit_connection<F: Fn() + 'static>(
    parent: Option<&gtk::Window>,
    e: ConnEntry,
    on_done: F,
) {
    entry_edit_dialog(parent, Some(&e), DlgMode::Edit, move |r| {
        let Some((draft, save, _connect)) = r else { return };
        if !save {
            return;
        }
        if !save_draft(&draft) {
            crate::panel::dialogs::show_error(
                None,
                tr("保存失败"),
                tr("写不进 connections.ini（目录权限或磁盘问题）。"),
            );
            return;
        }
        on_done();
    });
}

/// 转发规则来自哪套机制。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FwdSource {
    /// 进程内（跑在应用自己那条 SSH 连接上，终端标签页关了也照样在）
    InProc,
    /// 终端（交给终端标签页 spawn 的 ssh/plink 承接）
    Terminal,
}

/// 一条端口转发规则在树里的显示内容（两棵树共用）。
#[derive(Debug, Clone, PartialEq)]
pub struct FwdLeaf {
    /// 规则的内部形键（`L:9000:host:80`）——跟运行态的 `FwdView.key` 对得上
    pub key: String,
    /// 显示名（"本地 127.0.0.1:9000 → 10.0.0.1:80"）
    pub text: String,
    /// 来源标注（"进程内" / "终端"）
    pub meta: String,
    /// 悬停提示
    pub tip: String,
    /// 来自哪套机制
    pub source: FwdSource,
}

/// 某个 SSH 连接下面的端口转发规则（阶段 56）。
///
/// **纯逻辑**（不碰 GTK）：侧栏的会话树用这一份（阶段 77b 删掉连接管理器
/// 后，这里是转发规则的唯一来源）。
///
/// **为什么两类规则都列**：`forwards`（交给终端 spawn 的 ssh/plink 承接）与
/// `inproc_forwards`（跑在应用自己那条 SSH 连接上）是**两套独立机制**，可能
/// 同时存在。只列一套的话，用户会以为另一套没生效——灰字标清来源才不顶牛。
pub fn forward_leaves_for(e: &ConnEntry) -> Vec<FwdLeaf> {
    let mut out = Vec::new();
    for r in &e.inproc_forwards {
        let text = r.display();
        let meta = if r.enabled {
            tr("进程内").to_string()
        } else {
            tr("进程内 · 已停用").to_string()
        };
        out.push(FwdLeaf {
            key: format!("{}:{}", r.kind, r.spec),
            text: text.clone(),
            meta,
            tip: trf!("进程内转发：跑在应用自己这条 SSH 连接上（终端标签页关了也照样在）\n{text}\n来源：连接配置的「进程内端口映射」", text = text),
            source: FwdSource::InProc,
        });
    }
    for r in &e.forwards {
        let text = r.display();
        let meta = if r.enabled {
            tr("终端").to_string()
        } else {
            tr("终端 · 已停用").to_string()
        };
        out.push(FwdLeaf {
            key: format!("{}:{}", r.kind, r.spec),
            text: text.clone(),
            meta,
            tip: trf!("终端转发：由终端标签页的 ssh/plink 承接（-L/-R/-D）\n{text}", text = text),
            source: FwdSource::Terminal,
        });
    }
    out
}

/// 某条转发规则当前的启用状态（`None` = 规则不在列表里）。
pub fn fwd_enabled(entry: &ConnEntry, source: FwdSource, rule_key: &str) -> Option<bool> {
    let list = match source {
        FwdSource::InProc => &entry.inproc_forwards,
        FwdSource::Terminal => &entry.forwards,
    };
    list.iter()
        .find(|r| format!("{}:{}", r.kind, r.spec) == rule_key)
        .map(|r| r.enabled)
}

/// 启用/停用一条转发规则（**配置层**）：改规则本体的 `enabled` 并写回
/// connections.ini（下次连接自动带上）。
///
/// ⚠️ **只改配置，不碰运行中的会话**：运行层由调用方（两棵树的右键菜单）
/// 另外通知 worker —— 终端承接的规则由 ssh 进程管，运行态改不了，只能等
/// 下次连接生效。
pub fn set_fwd_enabled(
    entry: &ConnEntry,
    source: FwdSource,
    rule_key: &str,
    enabled: bool,
) -> Result<(), String> {
    set_fwd_enabled_to(&ini_path(), entry, source, rule_key, enabled)
}

/// [`set_fwd_enabled`] 的可注入路径版（测试不碰用户真实配置）。
pub fn set_fwd_enabled_to(
    path: &std::path::Path,
    entry: &ConnEntry,
    source: FwdSource,
    rule_key: &str,
    enabled: bool,
) -> Result<(), String> {
    match source {
        FwdSource::InProc => {
            let mut rules = entry.inproc_forwards.clone();
            if !flip_rule(&mut rules, rule_key, enabled) {
                return Err(tr("规则不在列表里").into());
            }
            let value = forward::to_storage_value(&rules);
            // 自建条目写它自己的组；ssh 别名写 [ssh_alias_forwards] 段（键 = 别名）
            let ok = if entry.custom {
                save_inproc_forwards_to(path, &entry.name, &value)
            } else {
                save_alias_forwards_to(path, &entry.name, &value)
            };
            if ok {
                Ok(())
            } else {
                Err(tr("写回 connections.ini 失败").into())
            }
        }
        FwdSource::Terminal => {
            // ssh config 别名的终端转发来自 ~/.ssh/config（只读）——不碰用户的 ssh 配置
            if !entry.custom {
                return Err(tr("ssh config 别名的转发规则在 ~/.ssh/config 里（只读），改不了").into());
            }
            let mut rules = entry.forwards.clone();
            if !flip_rule(&mut rules, rule_key, enabled) {
                return Err(tr("规则不在列表里").into());
            }
            let value = forward::to_storage_value(&rules);
            if save_forwards_to(path, &entry.name, &value) {
                Ok(())
            } else {
                Err(tr("写回 connections.ini 失败").into())
            }
        }
    }
}

/// 把规则列表里 key 匹配的那条翻成 enabled（找不到 = false）。
fn flip_rule(rules: &mut Vec<forward::FwdRule>, rule_key: &str, enabled: bool) -> bool {
    for r in rules.iter_mut() {
        if format!("{}:{}", r.kind, r.spec) == rule_key {
            r.enabled = enabled;
            return true;
        }
    }
    false
}

/// 条目 → 连接参数（只保留启用的转发；补默认端口/用户名/别名展开）。
pub fn entry_to_opts(e: &ConnEntry) -> ConnOptions {
    let mut o = e.to_opts();
    // 转发：只有启用的规则进会话；停用的保留在连接配置里，
    // 在连接配置里改（同一份规则集，1.4）
    o.forwards = forward::parse_rules(&forward::to_storage_value(
        &e.forwards.iter().filter(|r| r.enabled).cloned().collect::<Vec<_>>(),
    ))
    .into_iter()
    .map(|r| format!("{}:{}", r.kind, r.spec))
    .collect();
    fill_opts_defaults(&mut o);
    o
}

/// 这个条目的**会话键**（数据库连接按它分组）。
///
/// **为什么要用展开后的主机算**：ssh config 里的别名（`srv-01`）在真正连上
/// 之前会被展开成真实主机名——面板里的 opts 是展开后的，而条目本身还是
/// 别名。两边算出的会话键不一样的话，数据库连接就会"挂在这棵树里、却在那
/// 棵树上找不到"（用户建的连接，打开工作台后看不见）。
///
/// 自定义条目不受影响：它的键是 `s:<名字>`，别名展开不动它。
pub fn entry_session_key(e: &ConnEntry) -> String {
    let mut o = e.to_opts();
    if o.is_ssh() {
        fill_opts_defaults(&mut o);
    }
    crate::backend::dbstore::session_key(&o)
}

/// 按**会话键**找 SSH 连接条目（阶段 13：数据库连接是挂在某条 SSH 会话下的，
/// 反查它对应的会话参数）。
///
/// ⚠️ **只查连接列表，不反推**：`user@host:port` 这种键虽然能拆出地址，
/// 但堡垒机场景下端口/密码**下次就变了**（用户刻意不保存），按旧地址重连
/// 只会连到一台已经不是的机器上。所以查不到就是查不到——数据库连接跟着
/// 它**当时的**会话一起用，不去翻旧连接。
pub fn find_opts_by_session_key(key: &str) -> Option<ConnOptions> {
    for e in load_all() {
        if entry_session_key(&e) == key {
            return Some(entry_to_opts(&e));
        }
    }
    None
}

/// 表单结果 → 保存（一行一条的转发文本原样交给 parse_rules）。
fn save_draft(d: &EntryDraft) -> bool {
    let sercfg = if d.proto == "serial" {
        Some(d.sercfg.as_str())
    } else {
        None
    };
    let fwd = if d.proto == "ssh" {
        Some(d.forwards.as_str())
    } else {
        None
    };
    // 密码：勾了「记住密码」才存（阶段 70）
    let pw = if d.remember && !d.password.is_empty() {
        Some(d.password.as_str())
    } else {
        None
    };
    let ok = save_entry(
        &d.name,
        &d.host,
        d.port,
        Some(&d.user),
        pw,
        Some(&d.proto),
        sercfg,
        fwd,
    );
    if !ok {
        return false;
    }
    // 不记住密码 ⇒ 凭据库里那一份也删掉（用户说"别记住"就别留着）
    if !d.remember && crate::vault::is_unlocked() {
        crate::vault::remove(&d.name);
    }
    // 进程内端口映射是另一把键，单独回写（save_entry 不碰它，
    // 免得整条覆盖把并发修改抹掉）
    if d.proto == "ssh" {
        save_inproc_forwards(&d.name, &d.inproc_forwards);
    }
    // 会话默认设置同理：单独回写那几把键（阶段 69 之二）
    save_session_opts(&d.name, &d.sess);
    // 私钥与认证方式：也是单独那两把键（阶段 70）
    save_auth_opts(&d.name, d.keyfile.as_deref(), d.auth_method);
    true
}

/// 表单结果 → ConnEntry（保存与"仅连接"共用同一份字段搬运）。
fn draft_to_entry(d: &EntryDraft) -> ConnEntry {
    ConnEntry {
        name: d.name.clone(),
        host: d.host.clone(),
        user: if d.user.is_empty() {
            None
        } else {
            Some(d.user.clone())
        },
        password: if d.password.is_empty() {
            None
        } else {
            Some(Zeroizing::new(d.password.clone()))
        },
        port: d.port,
        custom: true,
        proto: d.proto.clone(),
        sercfg: if d.proto == "serial" {
            Some(d.sercfg.clone())
        } else {
            None
        },
        forwards: if d.proto == "ssh" {
            forward::parse_rules(&d.forwards)
        } else {
            Vec::new()
        },
        sess: d.sess.clone(),
        keyfile: d.keyfile.clone(),
        auth_method: d.auth_method,
        inproc_forwards: if d.proto == "ssh" {
            forward::parse_rules(&d.inproc_forwards)
        } else {
            Vec::new()
        },
    }
}

/// 「新建连接」：直接开新建表单。
///
/// `on_connect` 在「仅连接」/「保存并连接」时回调——通常是在当前窗口开新标签。
pub fn ui_new_connection<F: Fn(ConnOptions) + 'static>(
    parent: Option<&gtk::Window>,
    on_connect: F,
) {
    new_connection_dialog(parent, None, on_connect);
}

/// 「在连接中使用」（阶段 78）：新建连接窗口里**预选**这把私钥。
///
/// 私钥管理器刚生成完，用户多半就是想拿它建连接 —— 让他再手动去
/// 「认证」页的「私钥文件」下拉里挑一遍是白折腾。
/// 认证方式同时落在「只用私钥文件」上（阶段 70 的那把键）。
pub fn ui_new_connection_with_key<F: Fn(ConnOptions) + 'static>(
    parent: Option<&gtk::Window>,
    key_path: &str,
    on_connect: F,
) {
    // 用一条"只有私钥"的合成条目当初值：认证页把私钥选中、
    // 认证方式落在「只用私钥文件」上，其他字段留空。
    let synth = ConnEntry {
        name: String::new(),
        host: String::new(),
        user: None,
        password: None,
        port: 0,
        custom: true,
        proto: "ssh".to_string(),
        sercfg: None,
        forwards: Vec::new(),
        inproc_forwards: Vec::new(),
        sess: SessionOpts::default(),
        keyfile: Some(key_path.to_string()),
        auth_method: crate::args::AuthMethod::KeyFile,
    };
    new_connection_dialog(parent, Some(&synth), on_connect);
}

/// 新建连接窗口（`init` = 预置内容，`None` = 空白表单）。
fn new_connection_dialog<F: Fn(ConnOptions) + 'static>(
    parent: Option<&gtk::Window>,
    init: Option<&ConnEntry>,
    on_connect: F,
) {
    let on_connect = std::rc::Rc::new(on_connect);
    entry_edit_dialog(parent, init, DlgMode::NewConnect, move |r| {
        let Some((draft, save, connect)) = r else { return };
        if save && !save_draft(&draft) {
            crate::panel::dialogs::show_error(
                None,
                tr("保存失败"),
                tr("写不进 connections.ini（目录权限或磁盘问题）。"),
            );
            return;
        }
        let mut e = draft_to_entry(&draft);
        if !save {
            // 仅连接：不落盘，也就不该被"会话级覆盖"当成命名会话
            e.custom = false;
            e.name.clear();
        }
        if save {
            crate::sidebar::refresh();
        }
        if connect {
            on_connect(entry_to_opts(&e));
        }
    });
}

/// 别名解析 + 端口/用户默认值（侧边栏打开连接共用）。
pub fn fill_opts_defaults(o: &mut ConnOptions) {
    resolve_ssh_config_alias(o);
    if o.port == 0 {
        o.port = 22;
    }
    if o.user.is_empty() {
        o.user = glib::user_name().to_string_lossy().into_owned();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_ini(tag: &str) -> std::path::PathBuf {
        // ⚠ 目录**进程退出时删**（见 `util::testutil`）—— 以前只建不删，
        // 每跑一轮 `cargo test` 就在 /tmp 里留一批 `pp_connmgr_test_*`。
        crate::util::testutil::temp_dir("pp_connmgr_test", tag).join("connections.ini")
    }

    #[test]
    fn read_c_style_ini() {
        let path = tmp_ini("read");
        std::fs::write(
            &path,
            "[conn1]\nname=堡垒机\nhost=10.0.0.9\nport=2222\nuser=ops\npassword=secret\n\
             [conn2]\nname=空口令\nhost=web.corp\nport=22\n",
        )
        .unwrap();
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "堡垒机");
        assert_eq!(entries[0].host, "10.0.0.9");
        assert_eq!(entries[0].port, 2222);
        assert_eq!(entries[0].user.as_deref(), Some("ops"));
        assert_eq!(entries[0].password.as_ref().map(|p| p.as_str()), Some("secret"));
        assert!(entries[0].custom);
        assert_eq!(entries[1].password, None);
        assert_eq!(entries[1].port, 22);
    }

    /// 阶段 C12：**凭证库兜底**——快照里 password=None 的条目，在
    /// `to_opts` / `opts_for_recent` 组装连接参数这一刻能从库里拿到密码。
    ///
    /// 用户 2026-11-09 报：密码存在凭证库里、库也启用了，连接时还是让
    /// 输密码。根因：侧栏 / 欢迎页的 ConnEntry 是**启动时的快照**，那时
    /// 凭证库还锁着，而保存密码时 ini 的明文键已删 ⇒ 快照里恒为 None；
    /// 之后解锁了库，快照也不会自己更新。
    #[test]
    fn to_opts_and_recent_凭证库兜底() {
        // 隔离：PUTTYPLUS_VAULT 指到测试专用文件。
        let dir = crate::util::testutil::temp_dir("pp_connmgr_test", "vault_fb");
        let vf = dir.join("v.vault");
        // SAFETY：只有本测试读写 PUTTYPLUS_VAULT（其余测试都不碰这个变量）。
        unsafe { std::env::set_var("PUTTYPLUS_VAULT", &vf) };
        struct Reset(std::path::PathBuf);
        impl Drop for Reset {
            fn drop(&mut self) {
                // ⚠️ **先锁、再摘环境变量**（顺序反了会出事）：
                // 反过来的话，中间有个窗口——"库还开着、路径却已经切回
                // 用户真实凭据库"——别的测试正好在这会儿存密码，就会
                // **写进用户真实的凭据库**（明令禁止：测试不许碰用户配置）。
                crate::vault::lock(); // 解锁态只属于本测试，别泄漏给别的测试
                // SAFETY：同上。
                unsafe { std::env::remove_var("PUTTYPLUS_VAULT") };
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let _cleanup = Reset(vf.clone());

        // ini 里**没有**明文密码（保存时已进库、明文键已删）
        let path = tmp_ini("vault_fb");
        std::fs::write(
            &path,
            "[c1]\nname=测试条目\nhost=h.example\nport=22\nuser=ops\n",
        )
        .unwrap();

        // ① 快照时凭证库还没解锁（模拟启动时读列表）⇒ password None
        let e = load_custom_from(&path)[0].clone();
        assert_eq!(e.password, None, "库还没解锁 ⇒ 快照里不该有密码");
        assert_eq!(
            e.to_opts().password, None,
            "库还不存在 ⇒ 兜底查不到，走弹框路径"
        );

        // ② 建库（无主密码：unlock("") 秒过，不用跑 Argon2）+ 存密码 + 解锁
        crate::vault::create(
            "",
            &[("测试条目".into(), Zeroizing::new("pw123".into()))],
        )
        .expect("建库");
        crate::vault::unlock("").expect("解锁");

        // ③ **不重建列表**，直接用旧快照连：to_opts 这一刻现查库 ⇒ 拿到
        let o = e.to_opts();
        assert_eq!(o.password.as_ref().map(|p| p.as_str()), Some("pw123"));

        // ④ 最近连接同一条路：快照里有条目但密码 None ⇒ 兜底拿到
        let r = crate::recent::RecentConn {
            label: "最近".into(),
            host: "h.example".into(),
            port: 22,
            user: "ops".into(),
            ref_name: Some("测试条目".into()),
        };
        let o = crate::sidebar::opts_for_recent(&r, &[e.clone()]);
        assert_eq!(o.password.as_ref().map(|p| p.as_str()), Some("pw123"));
        assert_eq!(o.session_name.as_deref(), Some("测试条目"));

        // ⑤ 快照里**没有**这条（建连接晚于欢迎页构建）⇒ 直接查库
        let o = crate::sidebar::opts_for_recent(&r, &[]);
        assert_eq!(o.password.as_ref().map(|p| p.as_str()), Some("pw123"));
        assert_eq!(o.session_name.as_deref(), Some("测试条目"));

        // ⑥ 锁库 ⇒ 兜底拿不到 ⇒ 走原路（弹密码框）
        crate::vault::lock();
        assert_eq!(e.to_opts().password, None);
        assert_eq!(crate::sidebar::opts_for_recent(&r, &[]).password, None);
    }

    #[test]
    fn save_roundtrip_and_update() {
        let path = tmp_ini("save");
        assert!(save_entry_to(
            &path,
            "jump",
            "10.1.1.1",
            2222,
            Some("ops"),
            Some("pw1"),
            None,
            None,
            None
        ));
        let data = std::fs::read_to_string(&path).unwrap();
        assert!(data.contains("[conn1]"), "新组名从 conn1 起：{data}");
        assert!(data.contains("name=jump"));
        assert!(data.contains("port=2222"));

        // 同名保存 = 更新原组
        assert!(save_entry_to(
            &path, "jump", "10.1.1.2", 22, None, None, None, None, None
        ));
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].host, "10.1.1.2");
        assert_eq!(entries[0].port, 22);
        assert_eq!(entries[0].user, None);
        // ⚠️ **密码看 ini 的内容**，别看 `entries[0].password`：
        // 凭据库解锁时密码进的是**库**（不是 ini）⇒ `load_custom_from`
        // 读到的密码取决于"库此刻解没解锁"——并行跑时另一个测试正巧
        // 解锁了库，这个断言就翻车（实测 flaky，跑三挂一次）。
        // ini 里的明文键**两种情况都会被清掉**⇒ 看 ini 才稳。
        let data_pw = std::fs::read_to_string(&path).unwrap();
        assert!(
            !data_pw.contains("password="),
            "同名保存没把 ini 里的密码清掉：{data_pw}"
        );

        // 新条目 → conn2
        assert!(save_entry_to(
            &path, "web", "web.corp", 22, None, None, None, None, None
        ));
        let data2 = std::fs::read_to_string(&path).unwrap();
        assert!(data2.contains("[conn2]"));

        // 权限 600（POSIX 才有这个概念 ⇒ Windows 上跳过）
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path).unwrap().permissions();
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(mode.mode() & 0o777, 0o600);
        }
    }

    /// 进程内端口映射：独立一把键（inprocess_forwardings），
    /// 与交给终端 ssh 的 forwardings 互不干扰。
    #[test]
    fn inproc_forwards_persist_and_reload() {
        let path = tmp_ini("inproc_fwd");
        assert!(save_entry_to(
            &path,
            "jump",
            "10.1.1.1",
            2222,
            Some("ops"),
            None,
            None,
            None,
            Some("L:9000:host:80"),
        ));
        assert!(save_inproc_forwards_to(
            &path,
            "jump",
            "L:9100:host:81,D:1080\n!R:9200:host:22\nX:bad"
        ));
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        // 坏行 X:bad 丢掉，停用标记留住
        assert_eq!(e.inproc_forwards.len(), 3, "{:?}", e.inproc_forwards);
        assert_eq!(
            e.inproc_forwards[0],
            forward::FwdRule::new('L', "9100:host:81", true)
        );
        assert_eq!(
            e.inproc_forwards[1],
            forward::FwdRule::new('D', "1080", true)
        );
        assert_eq!(
            e.inproc_forwards[2],
            forward::FwdRule::new('R', "9200:host:22", false)
        );
        // 两把键互不干扰：forwardings 还在，进程内的另存
        assert_eq!(e.forwards.len(), 1);
        assert_eq!(e.forwards[0].kind, 'L');
        // to_opts 只带启用的进程内规则
        let o = e.to_opts();
        assert_eq!(o.inproc_forwards, vec!["L:9100:host:81", "D:1080"]);
        // 清空即删键
        assert!(save_inproc_forwards_to(&path, "jump", ""));
        assert!(load_custom_from(&path)[0].inproc_forwards.is_empty());
    }

    /// ssh config 别名的进程内转发规则：存在 ini 的单独一段里（键 = 别名），
    /// 别名条目读条目时自动带上，to_opts 也能带进会话。
    ///
    /// **这是堡垒机场景的关键**：用户平时连的都是 ~/.ssh/config 里的别名，
    /// 之前「保存到连接配置」只认自建条目 → 别名连接根本存不了规则。
    #[test]
    fn alias_forwards_persist_and_reload() {
        let path = tmp_ini("alias_fwd");
        assert!(save_alias_forwards_to(
            &path,
            "srv-01",
            "L:9100:host:81,D:1080\n!R:9200:host:22\nX:bad"
        ));
        // 规则落在 [ssh_alias_forwards] 段里，键 = 别名
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[ssh_alias_forwards]"),
            "别名规则要在单独一段里：{text}"
        );
        assert!(text.contains("srv-01="), "键是别名本身：{text}");

        let aliases = vec![crate::config::SshHostAlias {
            alias: "srv-01".into(),
            user: Some("root".into()),
            port: 22,
        }];
        let entries = load_all_with(&path, &aliases);
        let e = entries
            .iter()
            .find(|e| e.name == "srv-01")
            .expect("别名条目要在列表里");
        // 坏行丢掉，停用标记留住
        assert_eq!(e.inproc_forwards.len(), 3, "{:?}", e.inproc_forwards);
        assert_eq!(
            e.inproc_forwards[0],
            forward::FwdRule::new('L', "9100:host:81", true)
        );
        assert_eq!(
            e.inproc_forwards[2],
            forward::FwdRule::new('R', "9200:host:22", false)
        );
        // to_opts 只带启用的
        let o = e.to_opts();
        assert_eq!(o.inproc_forwards, vec!["L:9100:host:81", "D:1080"]);
    }

    /// 按会话键找「保存到连接配置」的目标：ssh 别名要能命中。
    #[test]
    fn find_fwd_target_hits_ssh_alias() {
        let entries = vec![ConnEntry {
            name: "srv-01".into(),
            // 已经是展开后的主机（别名展开在真实 ~/.ssh/config 里做，
            // 单测不依赖用户环境）
            host: "10.0.0.9".into(),
            user: Some("root".into()),
            password: None,
            port: 22,
            custom: false,
            proto: "ssh".into(),
            sercfg: None,
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        }];
        let key = crate::backend::dbstore::session_key(&{
            let mut o = crate::args::ConnOptions::default();
            o.host = "10.0.0.9".into();
            o.user = "root".into();
            o.port = 22;
            o
        });
        let got = find_fwd_save_target_in(&entries, &key, Some("srv-01"));
        assert_eq!(
            got,
            Ok(FwdSaveTarget::Alias("srv-01".into())),
            "别名连接要能存规则"
        );
    }

    /// 多个别名指向同一台机器：靠会话标题（= 条目名）区分；分不出来就
    /// 老实说「歧义」，别猜。
    #[test]
    fn find_fwd_target_ambiguous_aliases() {
        let mk = |name: &str| ConnEntry {
            name: name.into(),
            host: "10.0.0.9".into(),
            user: Some("root".into()),
            password: None,
            port: 22,
            custom: false,
            proto: "ssh".into(),
            sercfg: None,
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        };
        let entries = vec![mk("srv-01"), mk("prod")];
        let key = "root@10.0.0.9:22";
        // 有标题 → 挑得出来
        assert_eq!(
            find_fwd_save_target_in(&entries, key, Some("prod")),
            Ok(FwdSaveTarget::Alias("prod".into()))
        );
        // 没标题 → 歧义，不猜
        assert_eq!(
            find_fwd_save_target_in(&entries, key, None),
            Err(FwdTargetErr::Ambiguous(vec![
                "srv-01".into(),
                "prod".into()
            ]))
        );
        // 压根没有对应条目 → NoEntry（命令行直接开的会话）
        assert_eq!(
            find_fwd_save_target_in(&entries, "root@203.0.113.9:22", None),
            Err(FwdTargetErr::NoEntry)
        );
    }

    #[test]
    fn forwards_persist_and_reload() {
        let path = tmp_ini("fwd");
        assert!(save_entry_to(
            &path,
            "jump",
            "10.1.1.1",
            2222,
            None,
            None,
            None,
            None,
            // 坏条目与停用项一起存：坏行应被丢弃，停用标记要留住
            Some("L:9000:host:80\n!L:9001:host:81\nX:bad\nD:1080"),
        ));
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 1);
        let f = &entries[0].forwards;
        assert_eq!(f.len(), 3, "坏行 X:bad 不进列表：{f:?}");
        assert_eq!(f[0], forward::FwdRule::new('L', "9000:host:80", true));
        assert_eq!(f[1], forward::FwdRule::new('L', "9001:host:81", false));
        assert_eq!(f[2], forward::FwdRule::new('D', "1080", true));
        // 落盘值是规整后的内部形（换行折逗号）
        let data = std::fs::read_to_string(&path).unwrap();
        assert!(
            data.contains("forwardings=L:9000:host:80,!L:9001:host:81,D:1080"),
            "forwardings={data}"
        );
        // 清空即删键
        assert!(save_entry_to(
            &path, "jump", "10.1.1.1", 2222, None, None, None, None, Some(""),
        ));
        let data2 = std::fs::read_to_string(&path).unwrap();
        assert!(!data2.contains("forwardings"), "空规则应删键：{data2}");
        assert!(load_custom_from(&path)[0].forwards.is_empty());
    }

    #[test]
    fn forwards_survive_partial_update() {
        let path = tmp_ini("fwd_partial");
        assert!(save_entry_to(
            &path, "a", "h1", 22, Some("ops"), None, None, None, Some("L:9000:host:80"),
        ));
        // 只改 forwardings 不该动同组其他字段
        let kf = ini_load_from(&path);
        assert_eq!(kf.string("conn1", "name").ok().as_deref(), Some("a"));
        assert_eq!(kf.string("conn1", "user").ok().as_deref(), Some("ops"));
        assert_eq!(
            kf.string("conn1", "forwardings").ok().as_deref(),
            Some("L:9000:host:80")
        );
    }

    /// 会话默认设置（连接编辑器的「会话」页，阶段 69 之二）：
    /// 存进去读回来不变，而且 to_opts 会把它们带进会话。
    #[test]
    fn sess_opts_persist_and_reload() {
        let path = tmp_ini("sess");
        assert!(save_entry_to(
            &path,
            "jump",
            "10.1.1.1",
            2222,
            Some("ops"),
            None,
            None,
            None,
            None
        ));
        let opts = SessionOpts {
            keepalive_sec: Some(0),
            term_type: Some("screen-256color".into()),
            auth_order: Some(crate::args::AuthOrder::PublicKeyFirst),
            font_size: Some(12.0),
            bell: Some(false),
        };
        assert!(save_session_opts_to(&path, "jump", &opts));
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.sess, opts, "会话设置要原样读回来");
        // 也要带进会话参数（连接级默认值真的生效）
        let o = e.to_opts();
        assert_eq!(o.keepalive_sec, 0);
        assert_eq!(o.term_type.as_deref(), Some("screen-256color"));
        assert_eq!(o.auth_order, crate::args::AuthOrder::PublicKeyFirst);
        assert_eq!(o.font_size, Some(12.0));
        assert_eq!(o.bell, Some(false));
    }

    /// 等于内置默认的设置**不落盘**（缺键 = 内置默认，效果一样），
    /// 而且从"改过"改回默认时要把键删干净。
    #[test]
    fn sess_opts_defaults_not_written() {
        let path = tmp_ini("sess_def");
        assert!(save_entry_to(
            &path, "jump", "10.1.1.1", 2222, Some("ops"), None, None, None, None
        ));
        // 先写一组非默认值
        assert!(save_session_opts_to(
            &path,
            "jump",
            &SessionOpts {
                keepalive_sec: Some(0),
                term_type: Some("vt100".into()),
                auth_order: Some(crate::args::AuthOrder::PublicKeyFirst),
                font_size: Some(12.0),
                bell: Some(false),
            }
        ));
        let data = std::fs::read_to_string(&path).unwrap();
        assert!(data.contains("keepalive=0"), "{data}");
        // 再写回默认值 ⇒ 键要删掉（别在 ini 里留一堆"跟默认一样"的值）
        assert!(save_session_opts_to(&path, "jump", &SessionOpts::default()));
        let data2 = std::fs::read_to_string(&path).unwrap();
        for k in ["keepalive", "term=", "auth=", "font=", "bell"] {
            assert!(!data2.contains(k), "默认值不该落盘（{k}）：{data2}");
        }
        // 读回来是"没设"（= 内置默认）
        assert_eq!(load_custom_from(&path)[0].sess, SessionOpts::default());
    }

    /// 认证页的新键（阶段 70）：私钥与认证方式读回来要一样，
    /// 而且要带进会话参数（to_opts）。
    #[test]
    fn auth_opts_persist_and_reload() {
        let path = tmp_ini("auth");
        assert!(save_entry_to(
            &path, "jump", "10.1.1.1", 2222, Some("ops"), None, None, None, None
        ));
        assert!(save_auth_opts_to(
            &path,
            "jump",
            Some("/home/u/.ssh/id_ed25519"),
            crate::args::AuthMethod::KeyFile,
        ));
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.auth_method, crate::args::AuthMethod::KeyFile, "认证方式要原样读回");
        assert_eq!(e.keyfile.as_deref(), Some("/home/u/.ssh/id_ed25519"));
        // 也要带进会话参数（这条连接真的按它认证）
        let o = e.to_opts();
        assert_eq!(o.auth_method, crate::args::AuthMethod::KeyFile);
        assert_eq!(o.keyfile.as_deref(), Some("/home/u/.ssh/id_ed25519"));

        // 改回「自动」⇒ 那两把键删掉（缺键 = 自动）
        assert!(save_auth_opts_to(
            &path,
            "jump",
            None,
            crate::args::AuthMethod::Auto,
        ));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("keyfile"), "默认值不该落盘：{text}");
        assert!(!text.contains("auth="), "默认值不该落盘：{text}");
        let e2 = &load_custom_from(&path)[0];
        assert_eq!(e2.auth_method, crate::args::AuthMethod::Auto);
        assert_eq!(e2.keyfile, None);
    }

    /// ⚠️ 两把键在**同一个组**里，名字不能撞：
    /// `auth` 是**认证方式**（阶段 70），`authorder` 是**认证顺序**
    /// （阶段 69 之二）。撞了名就互相覆盖 ⇒ 一个刚设的认证方式会被
    /// 另一个的顺序值冲掉（或者反过来）。
    #[test]
    fn 认证方式与认证顺序不撞键() {
        let path = tmp_ini("auth_keys");
        assert!(save_entry_to(
            &path, "jump", "10.1.1.1", 2222, Some("ops"), None, None, None, None
        ));
        assert!(save_auth_opts_to(
            &path,
            "jump",
            None,
            crate::args::AuthMethod::Password,
        ));
        assert!(save_session_opts_to(
            &path,
            "jump",
            &SessionOpts {
                auth_order: Some(crate::args::AuthOrder::PublicKeyFirst),
                ..Default::default()
            },
        ));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("auth=password"), "{text}");
        assert!(text.contains("authorder=publickey-first"), "{text}");
        let e = &load_custom_from(&path)[0];
        assert_eq!(e.auth_method, crate::args::AuthMethod::Password);
        assert_eq!(
            e.sess.auth_order,
            Some(crate::args::AuthOrder::PublicKeyFirst)
        );
    }

    /// 串口/Telnet 条目曾经按 SSH 去连：proto 与 sercfg 没跟着搬，
    /// host 字段（串口是设备路径）被当成主机名。这里钉住。
    #[test]
    fn entry_to_opts_carries_proto_and_serial_conf() {
        let serial = ConnEntry {

            name: "交换机".into(),
            host: "/dev/ttyUSB0".into(),
            user: None,
            password: None,
            port: 0,
            custom: true,
            proto: "serial".into(),
            sercfg: Some("9600,7,e,2,x".into()),
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        };
        let o = serial.to_opts();
        assert_eq!(o.proto, "serial");
        assert_eq!(o.host, "/dev/ttyUSB0");
        assert_eq!(o.serial_baud, 9600);
        assert_eq!(o.serial_data, 7);
        assert_eq!(o.serial_parity, 'e');
        assert_eq!(o.serial_stop, 2);
        assert_eq!(o.serial_flow, 'x');
        assert!(!o.is_ssh());
        assert_eq!(o.session_name.as_deref(), Some("交换机"));

        // SSH 条目：sercfg 不该被误搬（那是串口专用）
        let ssh = ConnEntry {
            name: "堡垒机".into(),
            host: "10.0.0.9".into(),
            user: Some("ops".into()),
            password: None,
            port: 2222,
            custom: true,
            proto: "ssh".into(),
            sercfg: Some("9600,7,e,2".into()),
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        };
        let o2 = ssh.to_opts();
        assert!(o2.is_ssh());
        assert_eq!(o2.user, "ops");
        assert_eq!(o2.port, 2222);
        assert_eq!(o2.serial_baud, 115200, "SSH 条目的 sercfg 不生效");

        // proto 缺失（老 ini）按 ssh 处理，不留空串
        let legacy = ConnEntry {
            proto: String::new(),
            ..ssh
        };
        assert!(legacy.to_opts().is_ssh());
    }

    #[test]
    fn delete_entry_by_name() {
        let path = tmp_ini("del");
        save_entry_to(&path, "a", "h1", 22, None, None, None, None, None);
        save_entry_to(&path, "b", "h2", 22, None, None, None, None, None);
        assert!(delete_entry_from(&path, "a"));
        let entries = load_custom_from(&path);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "b");
        assert!(!delete_entry_from(&path, "不存在"));
    }

    /* ---- 阶段 56/57：转发叶子 + 活跃标记 ---- */

    fn 测试条目() -> ConnEntry {
        ConnEntry {
            name: "堡垒机".into(),
            host: "10.0.0.9".into(),
            user: Some("ops".into()),
            password: None,
            port: 2222,
            custom: true,
            proto: "ssh".into(),
            sercfg: None,
            forwards: vec![forward::FwdRule::new('L', "9000:10.0.0.1:80", true)],
            inproc_forwards: vec![
                forward::FwdRule::new('D', "1080", true),
                forward::FwdRule::new('R', "9200:h:22", false),
            ],
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        }
    }

    #[test]
    fn 转发叶子把两套机制都列出来() {
        let e = 测试条目();
        let leaves = forward_leaves_for(&e);
        assert_eq!(leaves.len(), 3, "{leaves:?}");
        // 进程内在前、终端在后
        assert_eq!(leaves[0].source, FwdSource::InProc);
        assert_eq!(leaves[0].key, "D:1080");
        assert!(leaves[0].text.contains("SOCKS"), "{leaves:?}");
        assert_eq!(leaves[0].meta, "进程内");
        // 停用的规则必须标出来（不然用户以为它在跑）
        assert_eq!(leaves[1].meta, "进程内 · 已停用");
        assert_eq!(leaves[1].key, "R:9200:h:22");
        // 终端承接的另一套
        assert_eq!(leaves[2].source, FwdSource::Terminal);
        assert_eq!(leaves[2].key, "L:9000:10.0.0.1:80");
        assert_eq!(leaves[2].meta, "终端");
    }

    /* ---- 阶段 58：右键菜单启停转发 ---- */

    #[test]
    fn 启停转发会写回配置() {
        let path = tmp_ini("toggle");
        std::fs::write(
            &path,
            "[conn1]\nname=跳板\nhost=10.0.0.9\nport=22\nuser=ops\n\
             forwardings=L:9000:10.0.0.1:80\ninprocess_forwardings=D:1080\n",
        )
        .unwrap();
        let entries = load_custom_from(&path);
        let e = entries[0].clone();
        assert_eq!(fwd_enabled(&e, FwdSource::InProc, "D:1080"), Some(true));
        assert_eq!(
            fwd_enabled(&e, FwdSource::Terminal, "L:9000:10.0.0.1:80"),
            Some(true)
        );

        // 停用进程内规则 → 写回 ini（下次连接带上停用标记）
        set_fwd_enabled_to(&path, &e, FwdSource::InProc, "D:1080", false).unwrap();
        let after = load_custom_from(&path);
        assert_eq!(fwd_enabled(&after[0], FwdSource::InProc, "D:1080"), Some(false));
        // 另一套（终端承接的）不受影响
        assert_eq!(
            fwd_enabled(&after[0], FwdSource::Terminal, "L:9000:10.0.0.1:80"),
            Some(true)
        );

        // 终端规则也能停用（写 forwardings 键）
        set_fwd_enabled_to(
            &path,
            &after[0],
            FwdSource::Terminal,
            "L:9000:10.0.0.1:80",
            false,
        )
        .unwrap();
        let after2 = load_custom_from(&path);
        assert_eq!(
            fwd_enabled(&after2[0], FwdSource::Terminal, "L:9000:10.0.0.1:80"),
            Some(false)
        );

        // 规则不在列表里 → 报错，而且**什么都没改**
        assert!(set_fwd_enabled_to(&path, &after2[0], FwdSource::InProc, "D:9999", false).is_err());
    }

    #[test]
    fn 别名的规则启停落在_alias_段() {
        let path = tmp_ini("toggle_alias");
        std::fs::write(&path, "[conn1]\nname=自建\nhost=10.0.0.9\nport=22\n").unwrap();
        let alias = ConnEntry {
            name: "srv-01".into(),
            host: "srv-01".into(),
            user: Some("root".into()),
            password: None,
            port: 22,
            custom: false,
            proto: "ssh".into(),
            sercfg: None,
            forwards: Vec::new(),
            inproc_forwards: vec![forward::FwdRule::new('D', "1080", true)],
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        };
        set_fwd_enabled_to(&path, &alias, FwdSource::InProc, "D:1080", false).unwrap();
        let kf = ini_load_from(&path);
        let v = kf.string(ALIAS_FWD_GROUP, "srv-01").unwrap_or_default();
        assert!(v.starts_with('!'), "停用的规则要带 ! 前缀：{v}");
        // ssh config 别名的**终端**转发在 ~/.ssh/config 里（只读）→ 明确拒绝
        let err = set_fwd_enabled_to(&path, &alias, FwdSource::Terminal, "L:9000:h:80", false)
            .unwrap_err();
        assert!(err.contains("只读"), "{err}");
    }

    // ⚠️ **别在 `cargo test --lib` 里写要建 GTK 控件的用例**：
    // 测试跑在 libtest 的子线程上，`gtk::init()` 在非主线程起不来 →
    // 就算侥幸起来了，也会**弄坏既有的 GTK 用例**（`theme::tests::跟随系统也能
    // 拿到修复规则`：一旦 GTK 初始化了，`gdk::Display::default()` 变成 Some，
    // 而无头环境下读不到系统色 → 那条用例反过来失败）。
    // → **要建控件的断言全放 example 冒烟**（`examples/fwd_tree_smoke.rs`：
    // 转发叶子挂在谁下面、活跃标记亮不亮，都在那里查）。
    /// 自 spawn 的参数形状：串口的**设备路径只能出现一次**，且走位置参数
    /// （阶段 95）。
    ///
    /// ⚠️ 以前 spawn 传的是 `-serial <设备>` **再加**一个位置参数
    /// `<设备>` ⇒ 位置参数那一轮撞上「host 已有值」⇒ 设备路径被当成
    /// **远程命令**发出去（串口会话一开就多跑一条命令）。
    #[test]
    fn 串口自spawn的设备路径只出现一次() {
        let e = ConnEntry {
            name: "串口".into(),
            host: "/dev/ttyUSB0".into(),
            user: None,
            password: None,
            port: 0,
            custom: true,
            proto: "serial".into(),
            sercfg: Some("115200,8n1".into()),
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        };
        let a = spawn_self_args(&e);
        assert!(a.contains(&"-serial".to_string()), "要有协议开关：{a:?}");
        assert_eq!(
            a.iter().filter(|s| s.as_str() == "/dev/ttyUSB0").count(),
            1,
            "设备路径只能出现一次（以前传了两次 ⇒ 第二个被当成远程命令）：{a:?}"
        );
        assert_eq!(
            a.last().map(String::as_str),
            Some("/dev/ttyUSB0"),
            "设备路径走位置参数：{a:?}"
        );

        // 用真正的解析器过一遍 ⇒ 设备是 host、**没有**远程命令
        let mut w = Vec::new();
        let home = std::path::Path::new("/tmp/pp_connmgr_no_such_home");
        let o = crate::args::ConnOptions::parse_impl(&a, home, None, &mut w);
        assert_eq!(o.proto, "serial");
        assert_eq!(o.host, "/dev/ttyUSB0");
        assert!(
            o.remote_cmd.is_none(),
            "设备路径不该被当成远程命令：{:?}",
            o.remote_cmd
        );
    }

    /// ssh / telnet 的自 spawn 形状：协议开关 + 末尾一个位置参数（主机名）。
    #[test]
    fn 自spawn的末尾是主机名() {
        let mut e = ConnEntry {
            name: "堡垒机".into(),
            host: "10.0.0.9".into(),
            user: Some("ops".into()),
            password: None,
            port: 2222,
            custom: true,
            proto: "ssh".into(),
            sercfg: None,
            forwards: Vec::new(),
            inproc_forwards: Vec::new(),
            sess: SessionOpts::default(),
            keyfile: None,
            auth_method: crate::args::AuthMethod::default(),
        };
        let a = spawn_self_args(&e);
        assert_eq!(
            a,
            vec!["-ssh", "-l", "ops", "-P", "2222", "-title", "堡垒机", "10.0.0.9"]
        );

        e.proto = "telnet".into();
        let a2 = spawn_self_args(&e);
        assert_eq!(a2.first().map(String::as_str), Some("-telnet"));
        assert_eq!(a2.last().map(String::as_str), Some("10.0.0.9"));
    }
}
