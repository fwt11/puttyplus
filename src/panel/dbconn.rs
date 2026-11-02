//! 数据库连接的**新建 / 编辑**对话框（阶段 13）。
//!
//! **为什么连接信息要搬出工作台**：工作台里那块表单（类型 / 地址 / 端口 /
//! 用户 / 库 / 密码）占掉整整四行，把对象树和结果网格挤得没地方站。连接信息
//! 属于"连接"这一层，不属于"数据库操作"这一层——所以它搬到连接管理器与
//! 会话树里，工作台只留一行「当前连接」。
//!
//! **对话框的字段顺序就是用户说的那个顺序**：
//!
//! 1. **数据库类型**（PostgreSQL / MySQL）——先定类型，后面的默认值跟着走；
//! 2. **在哪个会话上连**——数据库通道是**复用这条 SSH 连接**的
//!    （`direct-tcpip` 转发，不新建 SSH 连接），所以"在哪连"就是"挂在哪个
//!    SSH 会话下"，这决定了它在连接树里的位置；
//! 3. 名称 / 地址 / 端口 / 用户 / 库 / 密码。
//!
//! **保存即落盘**（`databases.ini` + 可选的凭据库），不像从前那样"连上了才
//! 存"——现在连接是**先建好再用**，工作台不再负责创建。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;

use crate::backend::db::DbProto;
use crate::backend::dbstore::DbProfile;
use crate::backend::dbstore;
use crate::panel::dialogs;
use zeroize::Zeroizing;
use crate::i18n::tr;
use crate::trf;

/// 一个待保存的数据库连接。
#[derive(Debug, Clone)]
pub struct DbConnDraft {
    /// 数据库类型
    pub proto: DbProto,
    /// 连接名（ini 里的主键）；空 = 自动生成
    pub name: String,
    /// 从**跳板机视角**看的主机（`127.0.0.1` = 跳板机回环）
    pub host: String,
    pub port: u16,
    pub user: String,
    pub dbname: String,
    /// 密码（`None` = 这次不设密码）
    pub password: Option<Zeroizing<String>>,
    /// 密码存进凭据库
    pub remember: bool,
    /// 挂在哪个 SSH 会话下（`databases.ini` 的分组键）
    pub session_key: String,
}

impl DbConnDraft {
    /// 落盘成一条连接配置。
    ///
    /// **连接名是主键**：留空就按「类型 地址:端口/库」自动生成一个。
    pub fn to_profile(&self) -> DbProfile {
        let name = if self.name.trim().is_empty() {
            DbProfile::default_name(self.proto, &self.host, self.port, &self.dbname)
        } else {
            self.name.trim().to_string()
        };
        DbProfile {
            name,
            proto: self.proto,
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            dbname: self.dbname.clone(),
            remember: self.remember,
        }
    }

    /// 校验；返回中文原因（`Ok` 通过）。
    pub fn validate(&self) -> Result<(), String> {
        if self.session_key.is_empty() {
            return Err(tr("请先选择「在哪个会话上连」").into());
        }
        if self.host.trim().is_empty() {
            return Err(tr("数据库地址不能为空").into());
        }
        if self.port == 0 {
            return Err(tr("端口无效（1-65535）").into());
        }
        if self.user.trim().is_empty() {
            return Err(tr("数据库用户不能为空").into());
        }
        Ok(())
    }
}

/// 把「**当场输入**的密码」放进**那个会话**的口袋（`PanelState.db_pass`）。
///
/// ⚠️ **为什么必须有这一步**（用户 2026-10-27 报的现象）：
/// 用户在「新建数据库连接」对话框里敲了密码，可**没勾**「记住密码」
/// → 密码**不落盘**（这是对的，不该偷偷存）→ 但**这次连接**也得用它啊！
/// 以前这里把密码直接丢了，工作台连的时候只能**再弹一次密码框**问用户
/// ——"刚才不是刚输过吗？"，看着就像密码框根本没生效。
///
/// 口袋是**按连接名**存的（见 `PanelState.db_pass`），所以同一个会话里
/// 多条连接各管各的密码。
///
/// **找不到对应会话就跳过**（那个会话没开标签页）：这时没有内存口袋可
/// 放，密码确实只能用一次——等用户打开那个会话的工作台时再问。
fn stash_password(session_key: &str, name: &str, pw: Option<&str>) {
    let Some(pw) = pw else { return };
    let Some(st) = crate::tabs::panel_state_by_session_key(session_key) else {
        return;
    };
    st.db_pass.borrow_mut().insert(
        name.to_string(),
        zeroize::Zeroizing::new(pw.to_string()),
    );
}

/// 存一条连接（配置进 ini，密码按需进凭据库）。
///
/// **返回一句提示**（可能为空）：密码没记上时要说清楚为什么——用户以为
/// 记住了、下次连却要重输，比一开始就说清楚糟糕得多。
pub fn save(draft: &DbConnDraft) -> Option<String> {
    let profile = draft.to_profile();
    let mut hint = None;
    if profile.remember {
        match &draft.password {
            None => hint = Some(tr("密码没填，这次不保存").to_string()),
            Some(pw) => {
                if !crate::vault::exists() {
                    // **要说清楚去哪儿办**：光说"没保存"，用户不知道该做什么
                    hint = Some(
                        tr("凭据库还没建，密码这次没保存。\n\n\
                         凭据库是本机存连接密码的地方（用主密码加密）。\
                         去顶栏菜单「凭据库…」设个主密码建一个，\
                         之后勾上「存进凭据库」就能存住。\n\n\
                         不建也能用——只是每次连库要重新输密码。")
                            .to_string(),
                    );
                } else if !crate::vault::is_unlocked() {
                    hint = Some(
                        tr("凭据库没解锁，密码这次没保存。\n\n\
                         去顶栏菜单「凭据库…」输入主密码解锁，再保存一次这条连接。")
                            .to_string(),
                    );
                } else if !crate::vault::put(
                    &dbstore::vault_name(&draft.session_key, &profile.name),
                    pw.as_str(),
                ) {
                    hint = Some(tr("密码写入凭据库失败（凭据库可能已满或被占用）").to_string());
                }
            }
        }
    }
    dbstore::save(&draft.session_key, &profile);
    // 会话树 / 连接管理器里要立刻能看到这条连接
    crate::sidebar::refresh();
    hint
}

/// 可选的会话（新建时让用户挑「在哪个会话上连」）。
///
/// 一行一个：`(会话键, 显示名)`。
/// 把表单内容装进滚动容器并限宽限高（阶段 73b，审计 P1-11）。
///
/// ⚠️ GTK4 的 CSS **没有** `max-width` / `max-height`（写了整条规则会被
/// 丢弃），Label 只能靠 `set_max_width_chars` 限宽；**容器**层面要限宽
/// 就得套滚动容器（`max-content-width` / `max-content-height`）。
fn form_in_scroller(content: &gtk::Box) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    sw.set_max_content_width(640);
    sw.set_max_content_height(520);
    sw.set_propagate_natural_width(true);
    sw.set_propagate_natural_height(true);
    sw.set_vexpand(true);
    sw.set_child(Some(content));
    sw
}

/// 这个会话是不是**堡垒机会话**（**没保存在连接列表里**）。
///
/// **为什么这么判**：堡垒机场景 = 用户**刻意不保存**会话（端口/密码下次
/// 就变，存下来是过期垃圾）⇒ 这条会话在连接列表里**找不到**。
/// 反过来，列表里有的会话 = 用户自己管理的普通连接。
///
/// ⚠️ 用户 2026-11-03：「对于堡垒机使用场景，**所有地方**都不要出现
///    『记住密码』」⇒ 这些会话里不摆那个勾选框（一次性会话的密码没有
///    "下次复用"可言，摆出来只会让人以为能存）。
pub fn is_bastion_session(session_key: &str) -> bool {
    !session_choices().iter().any(|(k, _)| k == session_key)
}

pub fn session_choices() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    // 同一台机器的「主机:端口:用户」——用来去重
    let mut hosts = std::collections::HashSet::new();
    // 自建 + ssh config 的连接条目
    for e in crate::connmgr::load_all() {
        let o = e.to_opts();
        if !o.is_ssh() {
            continue; // 数据库通道只复用 SSH 连接
        }
        let key = dbstore::session_key(&o);
        if !seen.insert(key.clone()) {
            continue;
        }
        hosts.insert(host_tag(&o.host, o.port, &o.user));
        let label = o
            .title
            .clone()
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| format!("{}@{}:{}", o.user, o.host, o.port));
        out.push((key, label));
    }
    // 最近用过的（可能不在连接库里，比如命令行连的）
    for r in crate::recent::load() {
        let key = format!("{}@{}:{}", r.user, r.host, r.port);
        if !seen.insert(key.clone()) {
            continue;
        }
        // ⚠️ **同一台机器别列两遍**：连接条目里的会话键是 `s:<名字>`，
        // 最近项的是 `user@host:port` —— 两个键不一样，但指向同一台机器。
        // 都列出来的话用户会看到「srv-01」和「srv-01」两条，以为是两台服务器。
        let port = if r.port > 0 { r.port } else { 22 };
        if hosts.contains(&host_tag(&r.host, port, &r.user)) {
            continue;
        }
        out.push((key, r.label));
    }
    out
}

/// 主机去重标签（主机 + 端口 + 用户）。
fn host_tag(host: &str, port: i32, user: &str) -> String {
    format!("{}|{}|{}", host, port, user)
}

/// 新建一个数据库连接（**挂在给定的会话上**）。
///
/// `preset_session`：宿主会话键，**定死** ——"从哪儿来就属于哪儿"。
/// 曾经这里还有个「在哪个会话上连」的下拉框让用户挑，现在**彻底没了**：
/// 所有入口都预置好会话了，要换宿主就切到别的 tab 再点「+」。
///
/// `on_done`：建好后回调（拿到的 `name` 是最终连接名——用户留空时是
/// 自动生成的那个，调用方要拿它去开工作台）。
/// 新建一条数据库连接。
///
/// ⚠️ **先测通再打开工作台**（`st` 给了会话状态时）：
/// 密码错了 / 没填 ⇒ **留在这个对话框里**，在**密码框**那儿提示，
/// **不打开工作台**。
///
/// **为什么**（用户 2026-11-02 报）：以前点「连接」就直接开工作台，
/// 连不上时工作台里再弹一个「请输入密码」的框——那个框**很容易被错过**，
/// 错过之后用户就不知道该怎么办了（工作台开着、里面是空的、状态栏一句
/// "连接失败"）。
///
/// ⇒ 原则：**操作失败要回到发起它的界面并提示**（用户原话）。
pub fn ui_new(
    parent: Option<&gtk::Window>,
    preset_session: &str,
    st: Option<Rc<super::PanelState>>,
    on_done: impl Fn(&str, &str) + 'static,
) {
    // 按钮是「连接」不是「保存」。
    //
    // **为什么**：所有入口（「+」、标签头右键、会话树右键、连接管理器
    // 右键）建完都**立刻连上并打开工作台**（`on_done` 里就是这么干的）。
    // 按钮写「保存」是**误导**——用户以为存个配置文件就完事了；可堡垒机
    // 场景下会话本身都没保存，"保存"既做不到也没意义。
    //
    // ⚠️ **照样落盘**：工作台是按会话键从 `databases.ini` 里列连接的
    // （`use_profile` → `dbstore::get`），不落盘这条连接建完就"找不到"，
    // 点了「连接」也连不上。落的是**连接定义**（地址/端口/用户/库名
    // ——数据库侧的东西，堡垒机换 SSH 端口也不影响），不是 SSH 会话。
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(tr("新建数据库连接（当前会话）"), parent, &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("连接"), gtk::ResponseType::Ok),
        ]);
    // 会话键要进 'static 的响应闭包 → 先 own 一份（preset_session 只是借用）
    let session_key = preset_session.to_string();
    dlg.set_default_response(gtk::ResponseType::Ok);
    // ⚠️ 限宽（阶段 73b，审计 P1-11）：以前**没设**默认宽度，而顶部那段说明
    // label 只 wrap 没限宽 ⇒ 自然宽度能到 1030px，小屏直接超出屏幕。
    dlg.set_default_size(560, -1);
    dialogs::style_dialog(&dlg);

    let box_ = dialogs::form_box();

    // ---- 1) 数据库类型 ----
    let proto = gtk::ComboBoxText::new();
    proto.append(Some("pg"), "PostgreSQL");
    proto.append(Some("mysql"), "MySQL");
    proto.set_active_id(Some("pg"));
    proto.set_tooltip_text(Some(tr("数据库类型；切换会按默认端口补全端口栏")));
    box_.append(&dialogs::form_row(tr("数据库类型"), &proto));

    // ---- 2) 在哪个会话上连 ----
    //
    // ⚠️ **两种进入方式，两种摆法**：
    //
    // - **带 preset**（从当前标签的「+」/ 右键菜单进来）：会话是**定死的**
    //   ——"从哪儿来就属于哪儿"。所以**不摆下拉框**，只摆一行说明：
    //   摆一个锁死的下拉框，用户会以为"得选一个会话"，可堡垒机场景下
    //   **当前会话往往根本没保存**（命令行连的、列表被清过），下拉框里
    //   压根没有它 → `set_active_id` 静默失效 → 框是空的、还锁着，
    //   用户彻底没法往下走。
    //
    // ⚠️ **这里曾经有过一个「在哪个会话上连」的下拉框，现在彻底没了**：
    //   所有入口（「+」、标签头右键、会话树右键、连接管理器右键）都把
    //   会话**预置死**了 ——"从哪儿来就属于哪儿"。要换会话就切到别的
    //   tab 再点「+」，这才是用户认的规矩。
    let mut text = trf!("这条连接挂在当前会话上（{preset_session}）。\n数据库通道复用这条 SSH 连接，不新建连接、不占本地端口。", preset_session = preset_session);
    // 当前会话**没保存在连接列表里**（堡垒机场景：命令行连的、列表被清过了）
    if !session_choices().iter().any(|(k, _)| k == preset_session) {
        // ⚠️ **不要**劝用户去连接管理器把这个会话存一条。
        // 堡垒机场景下用户**刻意不保存**（端口/密码下次就变，存下来是
        // 过期垃圾）。这条连接跟着**当前这个会话**走就够了：会话在，
        // 工作台里就看得见、用得上；会话没了，它就该跟着一起过期。
        text.push_str(
            tr("\n\n当前会话没有保存在连接列表里——不影响使用：\
             从这个会话打开工作台就能看到这条连接。\
             它跟着当前会话走，当前会话不保存，它也不会进连接列表。"),
        );
    }
    // ⚠️ 别用 markdown 星号：这是普通 gtk::Label（不是 markup label），
    // 星号会**原样显示**出来。
    let tip = gtk::Label::new(Some(&text));
    tip.add_css_class("pp-hint");
    tip.set_xalign(0.0);
    tip.set_wrap(true);
    // ⚠️ **必须限宽**（阶段 73b，审计 P1-11）：`wrap` 只按**分到的宽度**折行，
    // 而父容器默认按**自然宽**排版 ⇒ 自然宽 = 不折行时的整段长度 ⇒ wrap
    // 永远不触发，对话框被撑到 1030px。限了宽才真的折。
    tip.set_max_width_chars(60);
    box_.append(&tip);

    // ---- 3) 连接名 ----
    let name = gtk::Entry::new();
    name.set_placeholder_text(Some(tr("连接名（留空自动生成，同一会话下可存多个）")));
    box_.append(&dialogs::form_row(tr("连接名"), &name));

    // ---- 4) 地址 / 端口 ----
    let host = gtk::Entry::new();
    host.set_text("127.0.0.1");
    host.set_placeholder_text(Some(tr("数据库主机（从跳板机看）")));
    let port = gtk::Entry::new();
    port.set_text("5432");
    port.set_width_chars(7);
    let (hp, _, _) = dialogs::form_pair(tr("地址"), &host, tr("端口"), &port);
    box_.append(&hp);

    // ---- 5) 用户 / 数据库 ----
    let user = gtk::Entry::new();
    user.set_text("postgres");
    let dbname = gtk::Entry::new();
    dbname.set_text("postgres");
    let (ud, _, _) = dialogs::form_pair(tr("用户"), &user, tr("数据库"), &dbname);
    box_.append(&ud);

    // ---- 6) 密码 / 记住 ----
    let pass = gtk::Entry::new();
    pass.set_visibility(false);
    pass.set_placeholder_text(Some(tr("密码（可留空）")));
    let remember = gtk::CheckButton::with_label(tr("记住密码（存进凭据库）"));
    // ⚠️ 右边**不要再写一遍「记住密码」**（阶段 73b，审计 P1-11）：
    // 勾选框自带文案，再配一个同名标签就是同一行出现两次「记住密码」。
    // ⚠️ **凭据库存不住密码 ⇒ 整个不显示**（用户 2026-11-02）：勾了也存不住
    //    （`save()` 那头会拦下并说清楚）⇒ 摆个"点了没反应"的选项没用。
    //    判定只有一份（`vaultui::remember_policy`）⇒ 跟别处不会打架。
    // ⚠️ **堡垒机会话 ⇒ 也不显示**（用户 2026-11-03）：一次性会话的密码
    //    没有"下次复用"可言。
    let pol = crate::vaultui::remember_policy(crate::vault::state());
    remember.set_visible(pol.show_checkbox && !is_bastion_session(preset_session));
    remember.set_sensitive(pol.can_remember);
    let (pr, _, _) = dialogs::form_pair(tr("密码"), &pass, "", &remember);
    box_.append(&pr);

    let hint = gtk::Label::new(Some(""));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    box_.append(&hint);

    // 切换类型：端口 / 默认账号跟着走（只在还是默认值时改）
    {
        let port2 = port.clone();
        let user2 = user.clone();
        let dbname2 = dbname.clone();
        let proto2 = proto.clone();
        proto.connect_changed(move |_| {
            let p = proto2
                .active_id()
                .and_then(|s| DbProto::from_str(&s))
                .unwrap_or(DbProto::Postgres);
            let cur = port2.text().trim().to_string();
            if cur.is_empty() || cur == "5432" || cur == "3306" {
                port2.set_text(&p.default_port().to_string());
            }
            match p {
                DbProto::MySql => {
                    if user2.text().trim() == "postgres" {
                        user2.set_text("root");
                    }
                    if dbname2.text().trim() == "postgres" {
                        dbname2.set_text("");
                    }
                }
                DbProto::Postgres => {
                    if user2.text().trim() == "root" {
                        user2.set_text("postgres");
                    }
                    if dbname2.text().trim().is_empty() {
                        dbname2.set_text("postgres");
                    }
                }
            }
        });
    }

    // 套滚动容器 + 限宽限高（阶段 73b，审计 P1-11）
    dlg.content_area().append(&form_in_scroller(&box_));
    dlg.present();

    // ⚠️ **测连的结果**要跨过异步回包 ⇒ 放共享格里。
    //    （外层是 `Fn` 闭包 ⇒ 不能把 `draft` 移进内层闭包 —— 那是
    //    `FnOnce` 才干的事。）
    let pending: Rc<RefCell<Option<(DbConnDraft, DbProfile)>>> = Rc::new(RefCell::new(None));
    let testing = Rc::new(Cell::new(false));
    let on_done: Rc<dyn Fn(&str, &str)> = Rc::new(on_done);
    // 「存好 + 关窗 + 打开工作台」这一串（**测通了**才走）
    let finish: Rc<dyn Fn(&gtk::Dialog, &DbConnDraft, &DbProfile)> = {
        let on_done = Rc::clone(&on_done);
        Rc::new(
            move |dlg: &gtk::Dialog, draft: &DbConnDraft, profile: &DbProfile| {
                let saved = save(draft);
                // ⚠️ **这次连接就用刚敲的密码**（不勾「记住密码」也用）——
                // 否则工作台连的时候会**再问一遍**，看着像密码框没生效。
                stash_password(
                    &draft.session_key,
                    &profile.name,
                    draft.password.as_deref().map(|z| z.as_str()),
                );
                dlg.close();
                // 提示（密码没存上之类）用一次性对话框说清楚
                if let Some(h) = saved {
                    // 用户点的是「连接」——弹个「已保存」既误导（**还没连上**，
                    // 工作台正要开）又挡路（弹窗压在刚打开的工作台上）。
                    // 这里**只说密码那件事**：密码没记住，用户得知道。
                    dialogs::info(None, tr("密码没记住"), &h);
                }
                on_done(&draft.session_key, &profile.name);
            },
        )
    };

    dlg.connect_response(move |dlg, resp| {
        if resp != gtk::ResponseType::Ok {
            dlg.close();
            return;
        }
        if testing.get() {
            return; // 正在测，别重复提交
        }
        let proto = proto
            .active_id()
            .and_then(|s| DbProto::from_str(&s))
            .unwrap_or(DbProto::Postgres);
        // 会话是**定死**的（没有下拉框可选）
        let session_key = session_key.clone();
        let pw = pass.text().to_string();
        let draft = DbConnDraft {
            proto,
            name: name.text().trim().to_string(),
            host: host.text().trim().to_string(),
            port: port.text().trim().parse().unwrap_or(0),
            user: user.text().trim().to_string(),
            dbname: dbname.text().trim().to_string(),
            password: (!pw.is_empty()).then(|| Zeroizing::new(pw)),
            remember: remember.is_active(),
            session_key,
        };
        if let Err(e) = draft.validate() {
            hint.set_text(&format!("⚠ {e}"));
            return; // 不关对话框，让用户改
        }
        let profile = draft.to_profile();
        // ⚠️ **没有会话状态 ⇒ 没法测**（那个会话没开标签页）：
        //    照旧存了、打开工作台（工作台里会问密码）。
        // ⚠️ 外层是 `Fn` 闭包 ⇒ 不能把捕获的 `Rc` **移**出来（那是 FnOnce
        //    才干的事）⇒ 这里克隆一份（`Rc` 克隆很便宜）。
        let Some(st) = st.clone() else {
            finish(dlg, &draft, &profile);
            return;
        };
        // ---- 当场测连 ----
        //
        // ⚠️ **为什么不先打开工作台**：那样用户看到的是"工作台弹出来 +
        //    一个『请输入密码』的框"，那个框很容易被错过。
        //    ⇒ **先测通再开工作台**；不通就留在原地、在密码框那儿提示。
        testing.set(true);
        pass.remove_css_class("error");
        hint.set_text(tr("正在连接…（验证地址与密码）"));
        let cfg = crate::backend::db::DbConfig {
            proto: draft.proto,
            host: draft.host.clone(),
            port: draft.port,
            user: draft.user.clone(),
            dbname: draft.dbname.clone(),
            password: draft.password.clone(),
        };
        *pending.borrow_mut() = Some((draft, profile));
        let pending2 = Rc::clone(&pending);
        let testing2 = Rc::clone(&testing);
        let hint2 = hint.clone();
        let pass2 = pass.clone();
        let dlg2 = dlg.clone();
        let finish2 = Rc::clone(&finish);
        st.db_test(cfg, move |result, needs_password| {
            testing2.set(false);
            let Some((draft, profile)) = pending2.borrow_mut().take() else {
                return;
            };
            match result {
                Ok(()) => {
                    hint2.set_text("");
                    finish2(&dlg2, &draft, &profile);
                }
                Err(msg) => {
                    // ⚠️ **留在原地**，把原因写在**密码框**那儿
                    let wrong_pw = crate::backend::db::is_password_error(&msg);
                    let tip = if needs_password {
                        tr("⚠ 服务器要密码，但这条连接没有密码 —— 请在上面填写密码").to_string()
                    } else if wrong_pw {
                        trf!("⚠ 密码不对：{msg}", msg = msg)
                    } else {
                        trf!("⚠ 连接失败：{msg}", msg = msg)
                    };
                    hint2.set_text(&tip);
                    if needs_password || wrong_pw {
                        // 密码框标红 + 聚焦 + 选中（用户直接重敲就行）
                        pass2.add_css_class("error");
                        pass2.grab_focus();
                        if wrong_pw {
                            // 密码错的 ⇒ 把敲错的那个清掉
                            pass2.set_text("");
                        }
                    }
                }
            }
        });
    });
}

/// 编辑一条已存在的数据库连接。
///
/// `key` = 会话键，`name` = 连接名。
pub fn ui_edit(parent: Option<&gtk::Window>, key: &str, name: &str) {
    let Some(p) = dbstore::get(key, name) else {
        dialogs::show_error(parent, tr("数据库连接"), &trf!("找不到连接「{name}」。", name = name));
        return;
    };
    // ⚠️ 走 `windows::dialog`：必定挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(tr("编辑数据库连接"), parent, &[
            (tr("取消"), gtk::ResponseType::Cancel),
            (tr("保存"), gtk::ResponseType::Ok),
        ]);
    dlg.set_default_response(gtk::ResponseType::Ok);
    // ⚠️ 限宽（阶段 73b，审计 P1-11）：与「新建」同一个版式 ⇒ 同样限 520–640
    dlg.set_default_size(560, -1);
    dialogs::style_dialog(&dlg);

    let box_ = dialogs::form_box();

    let proto = gtk::ComboBoxText::new();
    proto.append(Some("pg"), "PostgreSQL");
    proto.append(Some("mysql"), "MySQL");
    proto.set_active_id(Some(p.proto.as_str()));
    box_.append(&dialogs::form_row(tr("数据库类型"), &proto));

    // 会话不能改：连接属于哪条 SSH 连接是它的"位置"，挪走等于删了重建
    // （历史分组键、凭据库键都跟着会话键走）。
    let sess_label = gtk::Label::new(Some(&key.to_string()));
    sess_label.set_xalign(0.0);
    sess_label.add_css_class("dim-label");
    sess_label.set_selectable(true);
    box_.append(&dialogs::form_row(tr("所属会话（不能改）"), &sess_label));

    let name_e = gtk::Entry::new();
    name_e.set_text(&p.name);
    box_.append(&dialogs::form_row(tr("连接名"), &name_e));

    let host = gtk::Entry::new();
    host.set_text(&p.host);
    let port = gtk::Entry::new();
    port.set_text(&p.port.to_string());
    port.set_width_chars(7);
    let (hp, _, _) = dialogs::form_pair(tr("地址"), &host, tr("端口"), &port);
    box_.append(&hp);

    let user = gtk::Entry::new();
    user.set_text(&p.user);
    let dbname = gtk::Entry::new();
    dbname.set_text(&p.dbname);
    let (ud, _, _) = dialogs::form_pair(tr("用户"), &user, tr("数据库"), &dbname);
    box_.append(&ud);

    let pass = gtk::Entry::new();
    pass.set_visibility(false);
    pass.set_placeholder_text(Some(tr("密码（留空表示不改）")));
    let remember = gtk::CheckButton::with_label(tr("记住密码（存进凭据库）"));
    // ⚠️ 右边不要再写一遍「记住密码」（阶段 73b，审计 P1-11）
    // ⚠️ **凭据库存不住密码 ⇒ 整个不显示**（用户 2026-11-02）：同上。
    // ⚠️ **堡垒机会话 ⇒ 也不显示**（用户 2026-11-03）：同上。
    let pol = crate::vaultui::remember_policy(crate::vault::state());
    // ⚠️ **堡垒机会话 ⇒ 连"带着勾"都不行**（用户 2026-11-03）：勾选框已经
    //    藏起来了，要是还留着 `active`，保存时就会**悄悄**把密码写进
    //    凭据库 —— 用户压根没勾过。
    let bastion = is_bastion_session(key);
    remember.set_visible(pol.show_checkbox && !bastion);
    remember.set_sensitive(pol.can_remember);
    // 存不住的时候不许"带着勾"（看着像已经记住了，其实压根没存）
    remember.set_active(p.remember && pol.can_remember && !bastion);
    let (pr, _, _) = dialogs::form_pair(tr("密码"), &pass, "", &remember);
    box_.append(&pr);

    let hint = gtk::Label::new(Some(""));
    hint.add_css_class("dim-label");
    hint.set_xalign(0.0);
    hint.set_wrap(true);
    box_.append(&hint);

    // 套滚动容器 + 限宽限高（阶段 73b，审计 P1-11）
    dlg.content_area().append(&form_in_scroller(&box_));
    dlg.present();

    let key2 = key.to_string();
    dlg.connect_response(move |dlg, resp| {
        if resp != gtk::ResponseType::Ok {
            dlg.close();
            return;
        }
        let proto = proto
            .active_id()
            .and_then(|s| DbProto::from_str(&s))
            .unwrap_or(DbProto::Postgres);
        let pw = pass.text().to_string();
        let draft = DbConnDraft {
            proto,
            name: name_e.text().trim().to_string(),
            host: host.text().trim().to_string(),
            port: port.text().trim().parse().unwrap_or(0),
            user: user.text().trim().to_string(),
            dbname: dbname.text().trim().to_string(),
            password: (!pw.is_empty()).then(|| Zeroizing::new(pw)),
            remember: remember.is_active(),
            session_key: key2.clone(),
        };
        if let Err(e) = draft.validate() {
            hint.set_text(&format!("⚠ {e}"));
            return;
        }
        let profile = draft.to_profile();
        // 改名 = 换主键：老名字那条要删掉，否则树里会多出一条
        let renamed = profile.name != p.name;
        if renamed {
            dbstore::remove(&key2, &p.name);
        }
        let saved = save(&draft);
        // 改完的密码**这次连接就用它**（不勾「记住密码」也用）——
        // 否则用户明明改了密码，下次连还要重输一遍。
        stash_password(
            &key2,
            &profile.name,
            draft.password.as_deref().map(|z| z.as_str()),
        );
        dlg.close();
        if let Some(h) = saved {
            dialogs::info(None, tr("连接已保存"), &trf!("「{v0}」已保存。\n\n{h}", v0 = profile.name, h = h));
        }
    });
}

/// 删除一条数据库连接（**二次确认**）。
///
/// 只删这一条连接的配置，不动它下面的表数据。
pub fn ui_remove(parent: Option<&gtk::Window>, key: &str, name: &str) {
    let label = trf!("删除数据库连接「{name}」？\n\n只删这条连接的配置（地址、账号、库名），不会动服务器上的数据。\n\n连接凭据也会一并删掉。", name = name);
    let key = key.to_string();
    let name = name.to_string();
    dialogs::confirm_with(parent, &label, tr("删除"), move |ok| {
        if !ok {
            return;
        }
        dbstore::remove(&key, &name);
        // 凭据库里的密码也删掉，别留孤儿
        let _ = crate::vault::remove(&dbstore::vault_name(&key, &name));
        crate::sidebar::refresh();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft() -> DbConnDraft {
        DbConnDraft {
            proto: DbProto::Postgres,
            name: String::new(),
            host: "127.0.0.1".into(),
            port: 5432,
            user: "postgres".into(),
            dbname: "postgres".into(),
            password: None,
            remember: false,
            session_key: "s:srv-01".into(),
        }
    }

    #[test]
    fn 连接名留空时自动生成() {
        let p = draft().to_profile();
        assert_eq!(p.name, "pg 127.0.0.1:5432/postgres");
    }

    #[test]
    fn 连接名用户填了就用用户的() {
        let mut d = draft();
        d.name = " 生产库 ".into();
        // 前后空白去掉（存进 ini 的主键不能带空格）
        assert_eq!(d.to_profile().name, "生产库");
    }

    #[test]
    fn 校验拦下空地址与空会话() {
        let mut d = draft();
        assert!(d.validate().is_ok());
        d.host = "  ".into();
        assert!(d.validate().is_err());
        let mut d2 = draft();
        d2.session_key = String::new();
        assert!(d2.validate().is_err());
        let mut d3 = draft();
        d3.port = 0;
        assert!(d3.validate().is_err());
        let mut d4 = draft();
        d4.user = String::new();
        assert!(d4.validate().is_err());
    }
}
