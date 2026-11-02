//! 关于对话框（4.2）。
//!
//! GTK4 没有 `GtkAboutDialog` 的绑定，所以自己拼一个：图标 + 名字 +
//! 版本 + 一句话定位 + 兼容性声明 + 依赖致谢。
//!
//! 版本号不写死在 UI 里：`env!("CARGO_PKG_VERSION")` 跟着 Cargo.toml 走，
//! 不会出现"改完版本号忘了改对话框"。

use std::cell::RefCell;

use gtk::prelude::*;

use crate::appicon;
use crate::i18n::tr;
use crate::trf;

pub fn set_parent(w: gtk::Window) {
    // ⚠️ 父窗口统一由 `windows` 管（阶段 67）：以前每个模块各存一份，
    // 换个入口弹出来的对话框可能挂在**别的**窗口下面。
    crate::windows::set_parent(w);
}

/// 弹出关于对话框。
pub fn open() {
    // ⚠️ 走 `windows::dialog`：**必定**挂上父窗口（阶段 67 / 审计 P2-16）
    let dlg = crate::windows::dialog(
        tr("关于 PuTTYPlus"),
        None,
        &[(tr("关闭"), gtk::ResponseType::Close)],
    );
    dlg.set_default_size(420, -1);
    dlg.connect_response(|dlg, _| dlg.close());

    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.set_margin_top(18);
    root.set_margin_bottom(18);
    root.set_margin_start(18);
    root.set_margin_end(18);

    // logo：直接用内嵌图标建纹理，不绕图标主题（about 是独立窗口）
    if let Some(tex) = appicon::texture() {
        let img = gtk::Image::from_paintable(Some(&tex));
        img.set_pixel_size(96);
        root.append(&img);
    }

    let title = gtk::Label::new(Some("PuTTYPlus"));
    title.set_markup("<span size='xx-large' weight='bold'>PuTTYPlus</span>");
    root.append(&title);

    // ⚠️ **许可证是 MIT**（2026-10-30，审计 P0-5）：仓库根 `LICENSE` 与
    // `rust/debian/copyright` 都是 MIT，这里原来写 GPL-2.0-or-later 属于
    // **用户可见的错误信息**（合规风险：用户按这行判断授权条款）。
    let ver = gtk::Label::new(Some(&trf!("版本 {v0} · MIT 许可", v0 = env!("CARGO_PKG_VERSION"))));
    ver.add_css_class("dim-label");
    root.append(&ver);

    let desc = gtk::Label::new(Some(
        tr("PuTTY 兼容的 SSH 终端 + SFTP 文件管理 + 数据库工作台"),
    ));
    root.append(&desc);

    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

    // ⚠️ 阶段 81（审计 P2-3）：这段原来在讲「**不是** Xshell / MobaXterm
    // 的复刻」「spawn 出的子进程 argv/环境变量一致」「C 版没有对应项」
    // ——全是**内部技术论证**：普通用户看不懂，也不关心（他只想知道
    // "这软件能干什么、能不能商用"）。⇒ 改成用户向文案。
    let claim = gtk::Label::new(None);
    claim.set_markup(
        tr("能干什么：\n\
         • <b>SSH 终端</b>：多标签分屏、按键广播、日志与录制回放\n\
         • <b>SFTP 文件管理</b>：上传下载、断点续传、目录镜像\n\
         • <b>数据库工作台</b>：PostgreSQL / MySQL 的对象浏览与查询\n\
         \n\
         命令行参数与 PuTTY 兼容，终端内转发、端口转发、串口等\
         由系统自带的 ssh / plink 承接。"),
    );
    claim.set_xalign(0.0);
    claim.set_wrap(true);
    root.append(&claim);

    let ack = gtk::Label::new(None);
    ack.set_markup(
        tr("<b>致谢</b>\n\
         终端组件：VTE · GTK4 · GLib\n\
         协议与凭据：russh · russh-sftp · ssh-key · argon2 · aes-gcm"),
    );
    ack.add_css_class("dim-label");
    ack.set_xalign(0.0);
    ack.set_wrap(true);
    root.append(&ack);

    dlg.content_area().append(&root);
    crate::panel::dialogs::style_dialog(&dlg);
    dlg.present();
}
