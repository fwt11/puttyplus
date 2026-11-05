//! GUI 设置持久化（~/.config/puttyplus/settings.ini）。
//! 首个用途：文件列表列显隐（远程/本地各自独立）。

use std::path::PathBuf;

use gtk::glib;
use gtk::prelude::*;
use crate::i18n::tr;

/// 某一面板的列显隐（name/icon 列固定显示，不在配置内）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColumnsCfg {
    pub size: bool,
    pub perm: bool,
    pub mtime: bool,
}

impl ColumnsCfg {
    fn defaults(_remote: bool) -> Self {
        // 默认显示「名称 + 大小 + 修改时间」：面板默认宽度 340 放得下，
        // 且这三列是选文件时最常看的；权限列默认关（远程才有，占地方）。
        ColumnsCfg {
            size: true,
            perm: false,
            mtime: true,
        }
    }
}

fn path() -> PathBuf {
    // 环境变量可覆盖：冒烟测试不该往用户的真实设置里塞东西
    // （比如"上次用的数据库连接"）。
    if let Some(p) = std::env::var_os("PUTTYPLUS_SETTINGS_INI") {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    glib::user_config_dir().join("puttyplus").join("settings.ini")
}

pub fn columns(remote: bool) -> ColumnsCfg {
    let kf = glib::KeyFile::new();
    if kf
        .load_from_file(path(), glib::KeyFileFlags::NONE)
        .is_err()
    {
        return ColumnsCfg::defaults(remote);
    }
    let group = if remote { "columns_remote" } else { "columns_local" };
    let d = ColumnsCfg::defaults(remote);
    let get = |key: &str, fallback: bool| -> bool {
        // 0/1；缺失或损坏时回退默认
        match kf.boolean(group, key) {
            Ok(v) => v,
            Err(_) => fallback,
        }
    };
    ColumnsCfg {
        size: get("size", d.size),
        perm: get("perm", d.perm),
        mtime: get("mtime", d.mtime),
    }
}

pub fn set_column(remote: bool, key: &str, on: bool) {
    let group = if remote { "columns_remote" } else { "columns_local" };
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    kf.set_boolean(group, key, on);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 窗口尺寸字符串解析() {
        assert_eq!(parse_win_size("1280x820"), Some((1280, 820)));
        assert_eq!(parse_win_size(" 1280 x 820 "), Some((1280, 820)));
        assert_eq!(parse_win_size("100x100"), None, "太小当没记过");
        assert_eq!(parse_win_size("0x0"), None);
        assert_eq!(parse_win_size("abc"), None);
        assert_eq!(parse_win_size("1280"), None);
    }

    // 注：glib KeyFile 依赖全局用户目录，这里只测默认值语义；
    // 读写往返属 GUI 集成（真机验证）。
    /// glib KeyFile 对键名的字符集有约束（会话键里有点、冒号、中文），
    /// 静默写失败会导致设置丢失，所以单独钉一个往返。
    #[test]
    fn session_override_survives_awkward_keys() {
        let keys = ["堡垒机", "10.0.0.9:2222", "a=b", "带 空格", "换\n行"];
        let mut kf = glib::KeyFile::new();
        for key in keys {
            kf.set_string(SESSION_GROUP, &field_key(key, "keepalive"), "15");
        }
        let text = kf.to_data();
        let mut kf2 = glib::KeyFile::new();
        kf2
            .load_from_data(&text, glib::KeyFileFlags::NONE)
            .expect("回读");
        for key in keys {
            assert_eq!(
                kf2.integer(SESSION_GROUP, &field_key(key, "keepalive"))
                    .ok(),
                Some(15),
                "键 {key:?} 写不进/读不回（safe_key={}）",
                safe_key(key)
            );
        }
        // 清洗会撞车的两个键必须各占一行，不能互相覆盖
        assert_ne!(safe_key("a:b"), safe_key("a_b"));
    }

    /// 最近连接上限的解析（纯逻辑，不碰文件）。
    #[test]
    fn 最近连接上限的解析() {
        assert_eq!(parse_recent_max(""), DEFAULT_RECENT_MAX, "缺键回默认");
        assert_eq!(parse_recent_max("abc"), DEFAULT_RECENT_MAX, "写坏了回默认");
        assert_eq!(parse_recent_max(" 5 "), 5);
        assert_eq!(parse_recent_max("20"), 20);
        assert_eq!(parse_recent_max("0"), 0, "0 = 不限");
    }

    /// 命令行启动的连接**默认不记**进最近连接（堡垒机场景）。
    #[test]
    fn 命令行启动默认不记最近连接() {
        // ⚠️ 这里只查**默认值常量**：`record_cli_recent()` 读的是设置文件，
        //    真机上用户自己可能开着 ⇒ 拿它当断言会误报。
        assert!(!RECORD_CLI_RECENT_DEFAULT, "默认不该记");
    }

    #[test]
    fn defaults_size_and_time() {        // 默认显示大小与修改时间，权限列默认关；远程/本地一致
        let r = ColumnsCfg::defaults(true);
        assert!(r.size && r.mtime && !r.perm);
        let l = ColumnsCfg::defaults(false);
        assert!(l.size && l.mtime && !l.perm);
    }

    /// 两层优先级（阶段 69 之二）：**连接默认值 < 会话覆盖**。
    ///
    /// ⚠️ 覆盖里**没设**的字段不能盖住连接默认值——
    /// 以前 `apply_session_override` 是无条件赋值（`o.font_size = ov.font_size`），
    /// 那时下面只有内置默认，无所谓；有了连接的默认值之后，
    /// 那种写法会把用户在连接里配好的字号/响铃/TERM 抹掉。
    #[test]
    fn 会话覆盖只盖住它自己设了的字段() {
        let mut o = crate::args::ConnOptions::default();
        // 下层 = 连接默认值
        o.font_size = Some(12.0);
        o.bell = Some(false);
        o.term_type = Some("screen-256color".into());
        o.keepalive_sec = 45;
        let base = o.clone();

        // 覆盖只设了字号 ⇒ 其他字段保持连接默认值
        let ov = SessionOverride {
            keepalive_sec: None,
            font_size: Some(14.0),
            bell: None,
            term_type: None,
            auth_order: None,
        };
        merge_session_override(&mut o, &ov);
        assert_eq!(o.font_size, Some(14.0), "覆盖里的字号要生效");
        assert_eq!(o.bell, Some(false), "没设的字段要保住连接默认值");
        assert_eq!(o.term_type.as_deref(), Some("screen-256color"));
        assert_eq!(o.keepalive_sec, 45);

        // 覆盖为空 ⇒ 连接默认值原样不动
        let mut o2 = base.clone();
        merge_session_override(&mut o2, &SessionOverride::default());
        assert_eq!(o2.font_size, base.font_size);
        assert_eq!(o2.bell, base.bell);
        assert_eq!(o2.term_type, base.term_type);
        assert_eq!(o2.keepalive_sec, base.keepalive_sec);

        // 保活的 0 = 显式关闭，必须能盖住下层
        let mut o3 = base.clone();
        merge_session_override(
            &mut o3,
            &SessionOverride {
                keepalive_sec: Some(0),
                ..Default::default()
            },
        );
        assert_eq!(o3.keepalive_sec, 0);
    }

    fn sample() -> Vec<QuickCmd> {
        vec![
            QuickCmd::new("服务", "重启 sshd", "systemctl restart sshd"),
            QuickCmd::new("服务", "看日志", "tail -n ${行数:100} ${路径:/var/log/syslog}"),
            QuickCmd::new("数据库", "看进程", "ps aux | grep ${名字:mysql}"),
        ]
    }

    #[test]
    fn 快速命令序列化往返() {
        let v = sample();
        let back = parse_quick_cmds(&serialize_quick_cmds(&v));
        assert_eq!(back, v);
    }

    #[test]
    fn 快速命令过_glib_ini_往返() {
        // 记录用换行分隔，靠 glib KeyFile 转义才写得进 ini
        let v = sample();
        let mut kf = glib::KeyFile::new();
        kf.set_string(QUICK_GROUP, QUICK_KEY, &serialize_quick_cmds(&v));
        let text = kf.to_data();
        let mut kf2 = glib::KeyFile::new();
        kf2
            .load_from_data(&text, glib::KeyFileFlags::NONE)
            .expect("回读");
        let got = parse_quick_cmds(&kf2.string(QUICK_GROUP, QUICK_KEY).unwrap());
        assert_eq!(got, v);
    }

    #[test]
    fn 快速命令_坏字段与制表符被洗掉() {
        // 字段不足的行丢掉，不牵连其它
        let data = "服务\t重启\tsystemctl restart sshd\n半截行\n\n数据库\t看\tps aux";
        let got = parse_quick_cmds(data);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].label, "重启");

        // 命令里带 TAB 会毁掉字段切分，落库前必须洗成空格
        let dirty = vec![QuickCmd {
            group: "组\t名".into(),
            label: "标\t签".into(),
            cmd: "printf\ta\\tb".into(),
        }];
        let back = parse_quick_cmds(&serialize_quick_cmds(&dirty));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].group, "组 名");
        assert_eq!(back[0].label, "标 签");
        assert_eq!(back[0].cmd, "printf a\\tb");
    }

    #[test]
    fn 快速命令_空分组与空命令() {
        assert_eq!(QuickCmd::new("", "", "ls").group, "未分组");
        assert_eq!(QuickCmd::new("", "", "ls").label, "命令");
        // 空命令没有意义，序列化时直接丢掉
        assert!(serialize_quick_cmds(&[QuickCmd::new("g", "l", "   ")]).is_empty());
        assert!(parse_quick_cmds("g\tl\t").is_empty());
    }
}

/// 侧边栏是否显示（[ui] show_sidebar，默认 true）。
/// 界面语言（存 `[general] language`）。
pub fn language() -> crate::i18n::Lang {
    crate::i18n::Lang::from_code(&get_str("general", "language"))
}

/// 保存界面语言。
pub fn set_language(l: crate::i18n::Lang) {
    set_str("general", "language", l.code());
}

/* ---------------- 最近连接（上限 / 命令行启动是否记录） ---------------- */

/// 最近连接默认留几条。
pub const DEFAULT_RECENT_MAX: usize = 10;

/// 最近连接最多留几条（`[general] recent_max`；**0 = 不限**）。
///
/// ⚠️ **堡垒机场景**：launcher 每次拉起都是一条一次性连接 ⇒ 「最近」
/// 列表很容易被冲垮。用户可以在首选项的「高级」页里改小（或设成不限）。
pub fn recent_max() -> usize {
    parse_recent_max(&get_str("general", "recent_max"))
}

/// 上限字符串的解析（**纯函数**，便于单测）。
///
/// ⚠️ 认不出来（缺键 / 写坏了）⇒ 回默认值，而不是当成 0（= 不限）：
/// 一个手滑写坏的值不该让「最近连接」无限增长。
fn parse_recent_max(v: &str) -> usize {
    match v.trim().parse::<usize>() {
        Ok(n) => n,
        Err(_) => DEFAULT_RECENT_MAX,
    }
}

/// 保存最近连接条数上限（0 = 不限）。
pub fn set_recent_max(n: usize) {
    set_str("general", "recent_max", &n.to_string());
}

/// 命令行启动的连接要不要记进「最近连接」——**默认值**。
///
/// ⚠️ 单独提成常量：单测只能查它（`record_cli_recent()` 读的是设置文件，
/// 真机上用户自己可能开着 ⇒ 拿它当断言会误报）。
pub const RECORD_CLI_RECENT_DEFAULT: bool = false;

/// 命令行启动的连接要不要记进「最近连接」（`[general] record_cli_recent`）。
///
/// ⚠️ **默认否**：堡垒机从外面拉起的一台设备一条记录 ⇒ 「最近」列表
/// 全是一次性连接，想找自己常用的那条得翻很久。
/// （界面上点开的连接不受影响 —— 那些照记。）
pub fn record_cli_recent() -> bool {
    get_bool("general", "record_cli_recent", RECORD_CLI_RECENT_DEFAULT)
}

/// 保存「命令行启动的连接是否记进最近连接」。
pub fn set_record_cli_recent(on: bool) {
    set_bool("general", "record_cli_recent", on);
}

pub fn show_sidebar() -> bool {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return true;
    }
    kf.boolean("ui", "show_sidebar").unwrap_or(true)
}

pub fn set_show_sidebar(on: bool) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    kf.set_boolean("ui", "show_sidebar", on);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/* ---------------- 窗口几何 / 列宽（P2） ---------------- */

/// 宽×高字符串解析（纯函数，便于单测）。太小/损坏一律当"没记过"。
fn parse_win_size(v: &str) -> Option<(i32, i32)> {
    let (w, h) = v.split_once('x')?;
    let w: i32 = w.trim().parse().ok()?;
    let h: i32 = h.trim().parse().ok()?;
    ((w >= 240 && h >= 180).then_some((w, h)))
}

/// 窗口尺寸记忆。只记宽高：GTK4 没有移动窗口的公开 API。
pub fn window_size(key: &str) -> Option<(i32, i32)> {
    parse_win_size(&get_str("windows", &format!("size_{key}")))
}

/// 关闭窗口时保存尺寸（最大化时不记，否则下次会以最大化尺寸开一个普通窗）。
pub fn save_window_size<W: IsA<gtk::Window>>(key: &str, win: &W) {
    let win: &gtk::Window = win.upcast_ref();
    if win.is_maximized() || win.is_fullscreen() {
        return;
    }
    let (w, h) = (win.width(), win.height());
    if w >= 240 && h >= 180 {
        set_str("windows", &format!("size_{key}"), &format!("{w}x{h}"));
    }
}

/// 文件列表的列宽（名称列始终 expand，不记）。
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColWidths {
    pub size: Option<i32>,
    pub perm: Option<i32>,
    pub mtime: Option<i32>,
}

pub fn col_widths(remote: bool) -> ColWidths {
    let group = if remote { "columns_remote" } else { "columns_local" };
    let get = |k: &str| -> Option<i32> {
        get_str(group, k).parse::<i32>().ok().filter(|v| *v >= 24)
    };
    ColWidths {
        size: get("size_w"),
        perm: get("perm_w"),
        mtime: get("mtime_w"),
    }
}

pub fn set_col_widths(remote: bool, w: ColWidths) {
    let group = if remote { "columns_remote" } else { "columns_local" };
    for (k, v) in [("size_w", w.size), ("perm_w", w.perm), ("mtime_w", w.mtime)] {
        if let Some(v) = v {
            set_str(group, k, &v.to_string());
        }
    }
}

/// 侧边栏宽度（主窗左右分割的位置）。
pub fn sidebar_width() -> i32 {
    get_str("ui", "sidebar_width")
        .parse::<i32>()
        .ok()
        // 旧版本可能存到 520：先读进来，运行期的 clamp_paned_start 再收到 320
        .filter(|v| (150..=520).contains(v))
        .unwrap_or(230)
}

pub fn set_sidebar_width(v: i32) {
    set_str("ui", "sidebar_width", &v.clamp(150, 320).to_string());
}

/// 给水平 Paned 的"起点侧"设上限：按当前宽度的比例 + 绝对上限，取小的那个；
/// 拖拽时 (`position`) 与窗口缩放时 (`max-position`) 都即时夹住。
///
/// 用途：SFTP 面板与会话侧边栏——终端应该占大部分窗口，列表够用就行。
pub fn clamp_paned_start(paned: &gtk::Paned, ratio: f64, hard_max: i32) {
    fn clamp(p: &gtk::Paned, ratio: f64, hard_max: i32) {
        let w = p.width();
        if w <= 0 {
            return;
        }
        let max = (((w as f64) * ratio) as i32).min(hard_max).max(200);
        if p.position() > max {
            p.set_position(max);
        }
    }
    paned.connect_notify_local(Some("position"), move |p, _| clamp(p, ratio, hard_max));
    paned.connect_notify_local(Some("max-position"), move |p, _| clamp(p, ratio, hard_max));
    let p = paned.clone();
    glib::timeout_add_local_once(std::time::Duration::from_millis(80), move || {
        clamp(&p, ratio, hard_max);
    });
}

/* ---------------- 会话级覆盖（1.5） ---------------- */

/// 单个会话的选项覆盖。`None` = 跟随内置默认，不写盘。
/// 键是会话名/标题，没有就用 `host:port`（见 ConnOptions::session_key）。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionOverride {
    /// 保活间隔（秒），0 = 关闭
    pub keepalive_sec: Option<u32>,
    /// 终端字号
    pub font_size: Option<f64>,
    /// 终端响铃
    pub bell: Option<bool>,
    /// 终端类型（TERM）；None = 内置默认 xterm-256color
    pub term_type: Option<String>,
    /// 认证优先级（2.2）；None = 默认密码优先（与 C 版一致）
    pub auth_order: Option<crate::args::AuthOrder>,
}

const SESSION_GROUP: &str = "session_overrides";

/// 字段名 → ini 键。会话键是用户起的名字，什么字符都可能有。
fn field_key(key: &str, field: &str) -> String {
    format!("{}:{field}", safe_key(key))
}

/// 会话键 → 可安全写进 ini 的键名。
///
/// glib KeyFile 只在 `set_string` 时静默拒绝非法键（实测 '=' 不行，
/// 而会话名叫 "a=b" 完全可能）——静默丢失比报错更难查，所以这里
/// 先清洗再补一段原键的哈希：清洗会撞车（"a:b" 与 "a_b" 都变成
/// a_b），哈希后缀保证不同会话各占一行。
fn safe_key(key: &str) -> String {
    let mut out = String::with_capacity(key.len() + 9);
    for ch in key.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ':') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out.push('-');
    out.push_str(&format!("{:08x}", fnv1a(key)));
    out
}

fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for b in s.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

pub fn session_override(key: &str) -> SessionOverride {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return SessionOverride::default();
    }
    SessionOverride {
        keepalive_sec: kf
            .integer(SESSION_GROUP, &field_key(key, "keepalive"))
            .ok()
            .map(|v| v.max(0) as u32),
        font_size: kf.double(SESSION_GROUP, &field_key(key, "font")).ok(),
        bell: kf.boolean(SESSION_GROUP, &field_key(key, "bell")).ok(),
        term_type: kf
            .string(SESSION_GROUP, &field_key(key, "term"))
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string()),
        auth_order: kf
            .string(SESSION_GROUP, &field_key(key, "auth"))
            .ok()
            .and_then(|s| crate::args::AuthOrder::parse(&s)),
    }
}

/// 写回覆盖（None 的字段删键，保持 ini 干净）。
/// 只动本会话的键，不碰同组其他会话。
pub fn set_session_override(key: &str, ov: &SessionOverride) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    let mut set_or_del = |field: &str, v: Option<String>| match v {
        Some(v) => {
            kf.set_string(SESSION_GROUP, &field_key(key, field), &v);
        }
        None => {
            let _ = kf.remove_key(SESSION_GROUP, &field_key(key, field));
        }
    };
    set_or_del("keepalive", ov.keepalive_sec.map(|v| v.to_string()));
    set_or_del(
        "font",
        ov.font_size.map(|v| format!("{v:.1}")),
    );
    set_or_del(
        "bell",
        ov.bell.map(|v| if v { "true" } else { "false" }.to_string()),
    );
    set_or_del("term", ov.term_type.clone());
    set_or_del("auth", ov.auth_order.map(|o| o.as_str().to_string()));
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/// 把覆盖写进连接参数（开新标签时调用）。
/// keepalive 的 0 是"显式关闭"，与 None（保持默认）要分得清，
/// 所以只在真的设了值时才覆盖。
pub fn apply_session_override(key: &str, o: &mut crate::args::ConnOptions) {
    merge_session_override(o, &session_override(key));
}

/// 把一份会话覆盖合并进连接参数（纯逻辑，好测）。
///
/// ⚠️ **只有覆盖里确实设了的字段才盖住下层**（阶段 69 之二）。
/// 下层现在是两层：连接的默认值（connections.ini 的「会话」页）→ 内置默认。
/// 以前这里是**无条件赋值**（`o.font_size = ov.font_size` …）——
/// 那时下面只有内置默认，无所谓；有了连接默认值之后，无条件赋值会把
/// 连接里设好的字号/响铃/TERM 抹成"没设"（用户明明在连接里配了，
/// 开出来却没生效）。
pub fn merge_session_override(o: &mut crate::args::ConnOptions, ov: &SessionOverride) {
    if let Some(k) = ov.keepalive_sec {
        o.keepalive_sec = k;
    }
    if let Some(f) = ov.font_size {
        o.font_size = Some(f);
    }
    if let Some(b) = ov.bell {
        o.bell = Some(b);
    }
    if let Some(t) = &ov.term_type {
        o.term_type = Some(t.clone());
    }
    if let Some(a) = ov.auth_order {
        o.auth_order = a;
    }
}

/// SFTP 面板（标签内左右分割）宽度；0 = 未设置（用内置默认）。
pub fn sftp_pane_pos() -> i32 {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return 0;
    }
    kf.integer("ui", "sftp_pane_pos").unwrap_or(0)
}

pub fn set_sftp_pane_pos(pos: i32) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    kf.set_integer("ui", "sftp_pane_pos", pos);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/* ---------------- 通用键值 ----------------
 * 主题 / 终端配色这类"一个字符串值"的设置都走这里，
 * 不为每个键单开一组函数——它们语义完全一样，只是 key 不同。
 */

/// 取一个"可能不存在"的字符串键。缺键与"键存在但值为空"要分得开——
/// 镜像的忽略规则就靠这个区分"用默认值"和"什么都不忽略"。
pub fn get_str_opt(group: &str, key: &str) -> Option<String> {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return None;
    }
    kf.string(group, key).ok().map(|s| s.to_string())
}

pub fn get_str(group: &str, key: &str) -> String {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return String::new();
    }
    kf.string(group, key).map(|s| s.to_string()).unwrap_or_default()
}

pub fn set_str(group: &str, key: &str, value: &str) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    kf.set_string(group, key, value);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/// 取一个布尔键（缺键 / 值损坏 ⇒ 回默认值）。
pub fn get_bool(group: &str, key: &str, default: bool) -> bool {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return default;
    }
    kf.boolean(group, key).unwrap_or(default)
}

/// 存一个布尔键。
pub fn set_bool(group: &str, key: &str, value: bool) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    kf.set_boolean(group, key, value);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/// 删掉一个键（键不存在也不算错）。
///
/// **为什么要有**：设置项要能"恢复默认"——把键删掉，读的时候自然落回
/// 默认值（比写一个"空 = 默认"的哨兵值干净）。
pub fn remove_str(group: &str, key: &str) {
    let p = path();
    let kf = glib::KeyFile::new();
    if kf.load_from_file(&p, glib::KeyFileFlags::NONE).is_err() {
        return;
    }
    if kf.remove_key(group, key).is_err() {
        return;
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/* ---------------- 终端外观（阶段 69：首选项中心） ---------------- */

/** 全局终端字号（**默认**）：连接 / 会话里单独设了的话以那边为准。
 *
 * 阶段 69 之前终端字号只有两个落点：连接里的 `font_size`、会话选项里的
 * 覆盖值——**没有全局默认** ⇒ 想"把终端字体调大"得逐条连接去改。
 * 现在首选项中心的「终端」页有全局默认（审计 P1-4：终端外观被拆在三处，
 * 用户没法在一个地方把终端字体调大、配色改成浅色）。
 */
pub fn term_font_size() -> Option<f64> {
    let s = get_str("ui", "term_font");
    s.trim().parse::<f64>().ok().filter(|v| *v > 0.0)
}

/// 设全局终端字号（`None` = 恢复默认）。
pub fn set_term_font_size(v: Option<f64>) {
    match v {
        Some(v) => set_str("ui", "term_font", &format!("{v:.1}")),
        None => remove_str("ui", "term_font"),
    }
}

/* ---------------- 数据库连接记忆（阶段 13） ---------------- */

const DB_LAST_GROUP: &str = "db_last";

/// 这个会话上次用的数据库连接名。
///
/// **为什么记这个**：阶段 13 之后工作台里只有一行「当前连接」，打开工作台
/// 时得知道该选哪一条——挑上次用的那条最省事（用户多半还在干同一件事）。
pub fn last_db_connection(key: &str) -> Option<String> {
    get_str_opt(DB_LAST_GROUP, &safe_key(key)).filter(|s| !s.is_empty())
}

/// 记住这个会话用了哪条数据库连接。
pub fn set_last_db_connection(key: &str, name: &str) {
    set_str(DB_LAST_GROUP, &safe_key(key), name);
}

/* ---------------- 本地目录记忆 ---------------- */

const NAV_GROUP: &str = "navigation";
const NAV_LOCAL_DIR: &str = "local_dir";

/// 上次浏览/关闭时的本地目录；没记过或记的是空串 → None。
pub fn last_local_dir() -> Option<String> {
    get_str_opt(NAV_GROUP, NAV_LOCAL_DIR).filter(|s| !s.is_empty())
}

/// 记录本地目录。调用方负责只在目录可用时写（这里不做存在性判断，
/// 避免导航路径短暂不可读时把旧值弄丢）。
pub fn set_last_local_dir(path: &str) {
    if !path.is_empty() {
        set_str(NAV_GROUP, NAV_LOCAL_DIR, path);
    }
}

/* ---------------- 目录镜像（6.2） ---------------- */

const MIRROR_GROUP: &str = "mirror";

/// 镜像的忽略规则。没设过 = 用内置默认；显式设成空串 = 什么都不忽略。
pub fn mirror_ignore() -> String {
    get_str_opt(MIRROR_GROUP, "ignore")
        .unwrap_or_else(|| crate::backend::mirror::DEFAULT_IGNORE.to_string())
}

pub fn set_mirror_ignore(v: &str) {
    set_str(MIRROR_GROUP, "ignore", v);
}

/* ---------------- 传输选项（3.5） ---------------- */

/// 断点续传：分片 + 源身份校验（默认开）。
pub fn xfer_resume() -> bool {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return true;
    }
    kf.boolean("xfer", "resume").unwrap_or(true)
}

pub fn set_xfer_resume(on: bool) {
    set_xfer_bool("resume", on);
}

/// 传输完成后比对 SHA-1（默认开）。
pub fn xfer_checksum() -> bool {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return true;
    }
    kf.boolean("xfer", "checksum").unwrap_or(true)
}

pub fn set_xfer_checksum(on: bool) {
    set_xfer_bool("checksum", on);
}

/// 最大并发传输文件数（默认 1 = 与旧行为一致的严格串行）。
/// 上限不是协议限制，只是防止用户把一个小水管/弱服务器拖死。
pub const XFER_PARALLEL_MAX: usize = 16;

pub fn xfer_parallel() -> usize {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return 1;
    }
    kf.integer("xfer", "parallel")
        .ok()
        .map(|v| (v.max(1) as usize).min(XFER_PARALLEL_MAX))
        .unwrap_or(1)
}

pub fn set_xfer_parallel(n: usize) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    let n = n.clamp(1, XFER_PARALLEL_MAX);
    kf.set_integer("xfer", "parallel", n as i32);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

fn set_xfer_bool(key: &str, on: bool) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    kf.set_boolean("xfer", key, on);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}

/* ---------------- 快速命令（3.3） ---------------- */

const QUICK_GROUP: &str = "quick_commands";
const QUICK_KEY: &str = "data";

/// 一条快速命令。`cmd` 里的 `${名字}` / `${名字:默认值}` 会在发送前
/// 弹窗问用户填（见 panel::quickcmd::parse_placeholders）。
#[derive(Debug, Clone, PartialEq)]
pub struct QuickCmd {
    pub group: String,
    pub label: String,
    pub cmd: String,
}

impl QuickCmd {
    pub fn new(group: &str, label: &str, cmd: &str) -> QuickCmd {
        QuickCmd {
            group: sanitize(group, tr("未分组")),
            label: sanitize(label, tr("命令")),
            cmd: cmd.trim_end().to_string(),
        }
    }
}

/// 记录之间用换行分隔、字段之间用 TAB。TAB 会毁掉解析，所以落库前一律洗掉。
fn sanitize(s: &str, empty: &str) -> String {
    let t = s.trim();
    if t.is_empty() {
        return empty.to_string();
    }
    t.chars()
        .map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c })
        .collect()
}

/// 解析 ini 里的 data 值。坏行（字段不足）跳过，不牵连其余。
pub fn parse_quick_cmds(data: &str) -> Vec<QuickCmd> {
    data.lines()
        .filter_map(|line| {
            let mut it = line.split('\t');
            let group = it.next()?;
            let label = it.next()?;
            let cmd = it.next().unwrap_or("");
            if cmd.is_empty() {
                return None;
            }
            Some(QuickCmd::new(group, label, cmd))
        })
        .collect()
}

/// 序列化成单行（glib KeyFile 会自己转义换行）。字段洗过，解析回来必等值。
pub fn serialize_quick_cmds(v: &[QuickCmd]) -> String {
    v.iter()
        .filter(|c| !c.cmd.trim().is_empty())
        .map(|c| {
            format!("{}\t{}\t{}",
                sanitize(&c.group, tr("未分组")),
                sanitize(&c.label, tr("命令")),
                c.cmd.replace(['\t', '\r', '\n'], " ")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn quick_commands() -> Vec<QuickCmd> {
    let kf = glib::KeyFile::new();
    if kf.load_from_file(path(), glib::KeyFileFlags::NONE).is_err() {
        return Vec::new();
    }
    match kf.string(QUICK_GROUP, QUICK_KEY) {
        Ok(s) => parse_quick_cmds(&s),
        Err(_) => Vec::new(),
    }
}

pub fn set_quick_commands(v: &[QuickCmd]) {
    let p = path();
    let kf = glib::KeyFile::new();
    let _ = kf.load_from_file(&p, glib::KeyFileFlags::NONE);
    if v.is_empty() {
        // 清空就删键，别在 ini 里留一条空串
        let _ = kf.remove_key(QUICK_GROUP, QUICK_KEY);
    } else {
        kf.set_string(QUICK_GROUP, QUICK_KEY, &serialize_quick_cmds(v));
    }
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(&p, kf.to_data().as_bytes());
}
