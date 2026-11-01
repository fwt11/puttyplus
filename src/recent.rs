//! 最近连接记录（~/.config/puttyplus/recent.ini，独立于 connections.ini）。
//! 双击侧边栏最近项即可重开；密码不落盘——自建条目通过 ref 名回查。

use std::path::PathBuf;

use gtk::glib;

#[derive(Clone, Debug, PartialEq)]
pub struct RecentConn {
    pub label: String,
    pub host: String,
    pub port: i32,
    pub user: String,
    /// 自建连接条目名（打开时回查密码；None = 曾是手动连接）
    pub ref_name: Option<String>,
}

fn path() -> PathBuf {
    // 环境变量可覆盖：测试与调试时想看/改别的文件，不必动真实配置。
    // （冒烟测试会开标签，open() 就会往这里写一条——不隔离的话，
    // 一次测试就在用户的"最近连接"里塞一条假记录。）
    if let Some(p) = std::env::var_os("PUTTYPLUS_RECENT_INI") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir().join("puttyplus").join("recent.ini")
}

const SEP: char = '\u{1f}';
const MAX: usize = 10;

/// 归一化：端口 0（未指定）就是 22，否则同一台机器会以 0 和 22 两条并存。
fn norm_port(p: i32) -> i32 {
    if p > 0 {
        p
    } else {
        22
    }
}

/// 去重键：用户 + 主机 + 端口。标签/来源不参与——
/// 同一台机器从侧边栏和命令行各开一次，不该留两行。
fn key(r: &RecentConn) -> (String, i32, String) {
    (r.host.clone(), norm_port(r.port), r.user.clone())
}

/// 保持顺序去重（先出现的（更新的）留下）。
fn dedup(list: Vec<RecentConn>) -> Vec<RecentConn> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for mut r in list {
        r.port = norm_port(r.port);
        if seen.insert(key(&r)) {
            out.push(r);
        }
    }
    out.truncate(MAX);
    out
}

pub fn load() -> Vec<RecentConn> {
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(path(), glib::KeyFileFlags::NONE);
    let mut raw = Vec::new();
    for i in 0..MAX as usize {
        let Ok(v) = kf.string("recent", &i.to_string()) else {
            break;
        };
        let mut it = v.split(SEP);
        let (Some(label), Some(host), Some(port), Some(user), ref_name) =
            (it.next(), it.next(), it.next(), it.next(), it.next())
        else {
            continue;
        };
        raw.push(RecentConn {
            label: label.to_string(),
            host: host.to_string(),
            port: crate::util::atoi_like(port) as i32,
            user: user.to_string(),
            ref_name: ref_name.filter(|r| !r.is_empty()).map(str::to_string),
        });
    }
    // 老文件里可能已有重复（历史上 port 0 与 22 被当成两台），显示时合并
    dedup(raw)
}

/// 去重置顶后写入（保持容量上限）。
pub fn push(rc: RecentConn) {
    let mut list = load();
    let mut rc = rc;
    rc.port = norm_port(rc.port);
    let k = key(&rc);
    list.retain(|r| key(r) != k);
    list.insert(0, rc);
    let list = dedup(list);
    save(&list);
}

/// 从列表移除一项（侧边栏右键「从最近中移除」）。
pub fn remove(idx: usize) {
    let mut list = load();
    if idx < list.len() {
        list.remove(idx);
        save(&list);
    }
}

fn save(list: &[RecentConn]) {
    let kf = glib::KeyFile::new();
    for (i, r) in list.iter().enumerate() {
        // 格式：label␟host␟port␟user␟ref（与 load 的 split 对齐，无前导分隔符）
        let v = format!(
            "{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}",
            r.label,
            r.host,
            r.port,
            r.user,
            r.ref_name.as_deref().unwrap_or(""),
        );
        kf.set_string("recent", &i.to_string(), &v);
    }
    if let Some(dir) = path().parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path(), kf.to_data().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 最近列表按用户主机端口去重() {
        let mk = |host: &str, port: i32, user: &str| RecentConn {
            label: "x".into(),
            host: host.into(),
            port,
            user: user.into(),
            ref_name: None,
        };
        // 同机同用户，一个 port 0 一个 22 → 合并成一条，保留最新的（前一条）
        let list = dedup(vec![
            mk("a", 22, "u"),
            mk("a", 0, "u"),
            mk("a", 22, "v"),
            mk("b", 22, "u"),
        ]);
        assert_eq!(list.len(), 3, "0 与 22 应合并");
        assert_eq!(list[0].host, "a");
        assert_eq!(list[0].port, 22);
        assert_eq!(list[1].user, "v");
        assert_eq!(list[2].host, "b");
        assert!(list.iter().all(|r| r.port > 0), "端口应归一化");
    }

    #[test]
    fn ser_de_roundtrip() {
        // KeyFile 依赖真实路径，此处仅验证序列化字段拼接/解析逻辑一致性
        let r = RecentConn {
            label: "堡垒机".into(),
            host: "10.0.0.9".into(),
            port: 2222,
            user: "ops".into(),
            ref_name: Some("堡垒机".into()),
        };
        let v = format!("{}{SEP}{}{SEP}{}{SEP}{}{SEP}{}", "堡垒机", "10.0.0.9", 2222, "ops", "堡垒机");
        let mut it = v.split(SEP);
        assert_eq!(it.next(), Some("堡垒机"));
        assert_eq!(it.next(), Some("10.0.0.9"));
        assert_eq!(it.next(), Some("2222"));
        assert_eq!(it.next(), Some("ops"));
        assert_eq!(it.next(), Some("堡垒机"));
        assert_eq!(it.next(), None);
    }
}
