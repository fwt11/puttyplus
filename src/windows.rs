//! 容器与窗口生命周期规范（阶段 67 / 审计 P2-17、P2-16）。
//!
//! 界面上装东西的容器一共**三类**，各有各的规矩（全项目统一按这个来，
//! 别再一处一个样）：
//!
//! | 容器 | 形态 | 规矩 |
//! |---|---|---|
//! | **页面** | 连接页里的**子标签** | 随会话走：终端 / 数据库 / 文件编辑器 |
//! | **工具窗** | 独立 `gtk::Window` | **非模态**、**单例**（同类只开一个）、**记尺寸** |
//! | **对话框** | `gtk::Dialog` | **模态**、**有父窗口**、短任务（干完就关） |
//!
//! **工具窗清单**（全局单例）：通知中心 / 端口转发 / 目录镜像 /
//! 凭据库 / 私钥。**会话级**的工具窗（传输队列、DB 搜索 / 历史 / 属性）
//! 挂在各自的会话上，天然单例，不进这里的登记表。
//!
//! **对话框清单**：新建连接 / 编辑连接 / DB 连接 / 导入导出 /
//! 确认 / 密码 / 关于 / 快捷键。
//!
//! ⚠️ **为什么要有这一层**：原来同一个工具能从多个入口开出**多个副本**
//! （审计 P2-17：用户看见两个窗口，第二个还盖着第一个），关闭行为
//! 也不统一（队列是 hide、别的是 destroy）、有的记尺寸有的不记。
//! ⇒ 现在：**工具窗走 [`open_or_focus`]**（同类只开一个 + 记尺寸），
//! **对话框走 [`dialog`]**（必定有父窗口）。

use std::cell::RefCell;
use std::collections::HashMap;

use gtk::prelude::*;
use crate::i18n::tr;

/// 工具窗的种类（**同类只开一个**）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ToolKind {
    /// 端口转发
    Forward,
    /// 目录镜像
    Mirror,
    /// 凭据库
    Vault,
    /// 私钥
    Key,
    /// 首选项中心（阶段 69）
    Settings,
    /// 通知中心（阶段 77c-3）
    Notify,
}

impl ToolKind {
    /// 设置里记尺寸用的键（`settings::window_size`）。
    pub fn size_key(self) -> &'static str {
        match self {
            ToolKind::Forward => "fwd",
            ToolKind::Mirror => "mirror",
            ToolKind::Vault => "vault",
            ToolKind::Key => "key",
            ToolKind::Settings => "settings",
            ToolKind::Notify => "notify",
        }
    }

    /// 窗口标题（**统一在这里改**，别各处手抄——抄本会随改名漂移）。
    pub fn title(self) -> &'static str {
        match self {
            ToolKind::Forward => tr("端口转发 — PuTTYPlus"),
            ToolKind::Mirror => tr("目录镜像 — PuTTYPlus"),
            ToolKind::Vault => tr("凭据库 — PuTTYPlus"),
            ToolKind::Key => tr("私钥 — PuTTYPlus"),
            ToolKind::Settings => tr("首选项 — PuTTYPlus"),
            ToolKind::Notify => tr("通知中心 — PuTTYPlus"),
        }
    }
}

thread_local! {
    /// 开着的工具窗。
    ///
    /// 存的是**强引用**：窗口关掉时 [`open_or_focus`] 装的 close-request
    /// 会把它从这儿摘掉 ⇒ 不会攒下一堆死窗口。
    // ⚠️ 不能用 `const { … }`：`HashMap::new()` 不是 const 函数
    static TOOLS: RefCell<HashMap<ToolKind, gtk::Window>> = RefCell::new(HashMap::new());

    /// 主窗口（对话框的默认父窗口）。
    static PARENT: RefCell<Option<gtk::Window>> =
        const { RefCell::new(None) };

    /// 建过的**没有父窗口**的对话框（冒烟断言用）。
    static DIALOGS_WITHOUT_PARENT: RefCell<Vec<String>> =
        const { RefCell::new(Vec::new()) };
}

/// 记下主窗口（对话框的默认父窗口）。
///
/// 主窗口建好时调一次（`main.rs`）。之后所有对话框都能挂到它下面——
/// 免得每个调用点都穿一遍 parent（也免得 GTK 报"无 transient parent"）。
pub fn set_parent(w: gtk::Window) {
    PARENT.with(|p| *p.borrow_mut() = Some(w));
}

/// 对话框的父窗口：**给了就用给的，没给就用主窗口**。
pub fn parent_of(given: Option<&gtk::Window>) -> Option<gtk::Window> {
    given
        .cloned()
        .or_else(|| PARENT.with(|p| p.borrow().clone()))
}

/// 打开（或聚焦）一个**工具窗**。
///
/// - **同类只开一个**：已经开着就 `present()`（聚焦）并把它返回去，
///   **不再开第二个**（审计 P2-17：用户看见两个连接管理器）；
/// - **非模态**：工具窗不阻塞主窗口（模态的请走 [`dialog`]）；
/// - **记尺寸**：关闭时把大小存进设置，下次按它开。
///
/// `build` 只在**第一次**打开时跑（重建窗口的事归它）。
pub fn open_or_focus<F: FnOnce() -> gtk::Window>(kind: ToolKind, build: F) -> gtk::Window {
    // 已经开着 ⇒ 聚焦它
    if let Some(w) = TOOLS.with(|m| m.borrow().get(&kind).cloned()) {
        w.present();
        return w;
    }
    let win = build();
    // 关掉 ⇒ 从登记表里摘掉（不然下次打开拿到的是已销毁的窗口）
    {
        let k = kind;
        win.connect_close_request(move |w| {
            crate::settings::save_window_size(k.size_key(), w);
            TOOLS.with(|m| {
                m.borrow_mut().remove(&k);
            });
            gtk::glib::Propagation::Proceed
        });
    }
    TOOLS.with(|m| m.borrow_mut().insert(kind, win.clone()));
    win.present();
    win
}

/// 这个工具窗开着吗（冒烟 / 调试用）。
pub fn is_open(kind: ToolKind) -> bool {
    TOOLS.with(|m| m.borrow().contains_key(&kind))
}

/// 某个工具窗当前开着的话，把它拿回来（没有则 `None`）。
pub fn tool_window(kind: ToolKind) -> Option<gtk::Window> {
    TOOLS.with(|m| m.borrow().get(&kind).cloned())
}

/// 当前登记了几个工具窗（冒烟用）。
pub fn open_count() -> usize {
    TOOLS.with(|m| m.borrow().len())
}

/// 建一个**对话框**：模态 + **必定挂上父窗口**。
///
/// ⚠️ 没有父窗口时**记一笔**（[`dialogs_without_parent`] 会列出来）——
/// "无父窗口的模态框"在多窗口下会浮在别的窗口上面，甚至被主窗口挡住
/// （审计 P2-16）。
pub fn dialog(
    title: &str,
    parent: Option<&gtk::Window>,
    buttons: &[(&str, gtk::ResponseType)],
) -> gtk::Dialog {
    let p = parent_of(parent);
    if p.is_none() {
        DIALOGS_WITHOUT_PARENT.with(|d| d.borrow_mut().push(title.to_string()));
    }
    gtk::Dialog::with_buttons(Some(title), p.as_ref(), gtk::DialogFlags::MODAL, buttons)
}

/// 建一个**文件选择对话框**：**模态** + 必定挂上父窗口。
///
/// ⚠️ **为什么文件选择器必须模态**（用户 2026-11-14 报的 bug）：
/// GTK4 里模态对话框会**抓走输入**（grab）。这时再打开一个**非模态**
/// 窗口，它虽然能显示出来，却**收不到任何点击** ⇒ 用户看到的是
/// 「文件选择器弹出来了，可点文件列表没反应、也选不了」。
/// 探针实测（`/tmp/gtk_file_probe.c`）：同样两个窗口，
/// 选择器非模态 ⇒ `SEL=<none>`；选择器模态 ⇒ 选中成功。
///
/// ⇒ 铁律：**最后打开的对话框必须是模态的**（要么自己 `modal`，
/// 要么挂在模态父窗口下面——两者都行，但**不能两个都不是**）。
///
/// ⚠️ **别再各处手搓 `gtk::FileChooserDialog`**（漏了 `.modal(true)`
/// 就会出上面那个问题）——全项目统一走这里。
pub fn file_chooser_dialog(
    title: &str,
    parent: Option<&gtk::Window>,
    action: gtk::FileChooserAction,
) -> gtk::FileChooserDialog {
    let p = parent_of(parent);
    if p.is_none() {
        DIALOGS_WITHOUT_PARENT.with(|d| d.borrow_mut().push(title.to_string()));
    }
    let mut b = gtk::FileChooserDialog::builder()
        .title(title)
        .action(action)
        // ⚠️ 这一行是本函数存在的理由（漏了 ⇒ 对话框点不动）
        .modal(true);
    if let Some(p) = p.as_ref() {
        b = b.transient_for(p);
    }
    b.build()
}

/// 建过的**没有父窗口**的对话框（冒烟断言用）。
pub fn dialogs_without_parent() -> Vec<String> {
    DIALOGS_WITHOUT_PARENT.with(|d| d.borrow().clone())
}

/// 清掉"没有父窗口"的记账（冒烟分段跑时用）。
pub fn reset_dialog_audit() {
    DIALOGS_WITHOUT_PARENT.with(|d| d.borrow_mut().clear());
}
