//! 统一弹层菜单构件（终端 / 侧边栏 / 文件面板 / 队列 / 标签 / 连接管理）。
//!
//! 全是 `Popover` + `Button`，不是 `PopoverMenu`：需要在行里放图标、快捷键、
//! 右侧勾选，还要按行类型决定危险色，`gio::Menu` 模型表达不了这些。
//!
//! 观感由 `theme.rs` 的 `.pp-menu-*` 类提供（圆角卡片、行高、hover、投影），
//! 颜色全部走 `@pp_*` 令牌，深色/浅色/高对比/跟随系统一致。

use gtk::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// 菜单主体：竖向排列、最小宽度与内边距由 CSS（`.pp-menu-body`）给。
pub fn body() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 0);
    b.add_css_class("pp-menu-body");
    b
}

/// 建一个统一风格的弹层，**不挂父**。
///
/// 给 `gtk::MenuButton::set_popover()` 用：那个方法自己就会
/// `gtk_popover_set_parent`（把弹层挂到按钮上），再手动 `set_parent`
/// 一次会报
/// `gtk_widget_set_parent: assertion 'parent == NULL' failed`。
///
/// 需要「弹层挂在某个 widget 上、用 `set_pointing_to` 定位」的场景
/// （右键菜单、TreeView 上的列菜单）请用 [`popover`]。
pub fn popover_detached() -> gtk::Popover {
    let pop = gtk::Popover::new();
    pop.add_css_class("pp-menu");
    pop.set_has_arrow(false);
    pop.set_position(gtk::PositionType::Bottom);
    pop.set_offset(0, 4);
    pop
}

/// 建一个统一风格的弹层，并挂到 `parent` 上。
///
/// 调用方负责 `popup()` / `set_pointing_to()`；这里只定共性：
/// 无箭头、向下弹、与锚点留 4px 间隙、带 `pp-menu` 类。
pub fn popover(parent: &impl IsA<gtk::Widget>) -> gtk::Popover {
    let pop = popover_detached();
    pop.set_parent(parent);
    pop
}

/// 分组标题（“显示列”“常用命令”这类），比菜单项小一号、弱化。
pub fn section(title: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(title));
    l.set_xalign(0.0);
    l.add_css_class("pp-menu-section");
    l
}

/// 菜单的一行：图标 + 左对齐文字 +（可选）快捷键提示 +（可选）勾选标记。
///
/// `reserve_check` 为真时即使未勾选也占住 16px：同时有快捷键与勾选的开关行
/// 不占位的话，菜单在开/关两种状态下宽度不同，重新弹出时会“跳一下”。
fn row(
    icon: &str,
    label: &str,
    accel: Option<&str>,
    reserve_check: bool,
) -> (gtk::Box, Option<gtk::Image>) {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    img.add_css_class("pp-menu-icon");
    row.append(&img);
    let l = gtk::Label::new(Some(label));
    l.set_xalign(0.0);
    l.set_hexpand(true);
    l.add_css_class("pp-menu-label");
    row.append(&l);
    if let Some(a) = accel {
        let al = gtk::Label::new(Some(a));
        al.add_css_class("pp-menu-accel");
        row.append(&al);
    }
    if !reserve_check {
        return (row, None);
    }
    let check = gtk::Image::from_icon_name("object-select-symbolic");
    check.set_pixel_size(16);
    check.add_css_class("pp-menu-check");
    check.set_opacity(0.0);
    row.append(&check);
    (row, Some(check))
}

/// 普通菜单项（点击执行动作）。调用方自己连 `clicked` 并收起弹层。
pub fn item(icon: &str, label: &str, accel: Option<&str>) -> gtk::Button {
    let b = gtk::Button::new();
    b.add_css_class("pp-menu-item");
    let (r, _) = row(icon, label, accel, false);
    b.set_child(Some(&r));
    attach_hover(&b);
    b
}

/// 改菜单项的文字（[`item`] / [`toggle`] 建的项）。
///
/// **为什么要有**：菜单标题要跟着状态变（凭据库菜单项显示
/// 「凭据库…（已解锁）」）。`gtk::Button` 的 child 是一层 Box，
/// 得钻进去把带 `pp-menu-label` 类的那个 Label 找出来。
pub fn set_item_label(b: &gtk::Button, text: &str) {
    if let Some(l) = label_of(b) {
        l.set_text(text);
    }
}

/// 菜单项的文字（[`item`] / [`toggle`] 建的项）。
pub fn item_label(b: &gtk::Button) -> Option<String> {
    label_of(b).map(|l| l.text().to_string())
}

/// 在菜单项的行里找到那个文字 Label。
fn label_of(b: &gtk::Button) -> Option<gtk::Label> {
    let child = b.child()?;
    let row = child.downcast_ref::<gtk::Box>()?;
    let mut cur = row.first_child();
    while let Some(c) = cur {
        if let Some(l) = c.downcast_ref::<gtk::Label>() {
            if l.css_classes().iter().any(|x| x == "pp-menu-label") {
                return Some(l.clone());
            }
        }
        cur = c.next_sibling();
    }
    None
}

/// 菜单项的**图标名**（[`item`] 建的项）。
///
/// **为什么要有**：菜单项的图标是"这个入口长什么样"的**唯一**来源
/// （阶段 68 的「新建」入口统一）⇒ 冒烟要能验"图标真的是表里那个"，
/// 不能只数按钮个数。
pub fn item_icon(b: &gtk::Button) -> Option<String> {
    let child = b.child()?;
    let row = child.downcast_ref::<gtk::Box>()?;
    let mut cur = row.first_child();
    while let Some(c) = cur {
        if let Some(img) = c.downcast_ref::<gtk::Image>() {
            if img.css_classes().iter().any(|x| x == "pp-menu-icon") {
                return img.icon_name().map(|s| s.to_string());
            }
        }
        cur = c.next_sibling();
    }
    None
}

/// 显式维护 hover 类，不依赖 GTK 的 `:hover` 状态。
///
/// 实测在 `GtkPopover` 里（尤其是文件面板挂在 `TreeView` 上的弹层），
/// 按钮拿到 `PRELIGHT` 状态后 CSS `:hover` 仍不刷背景；显式加/删类
/// 在真实桌面和 Xvfb 下都稳定。菜单只有十几行，一个 Motion 控制器
/// 的开销可以忽略。
fn attach_hover<W: IsA<gtk::Widget>>(w: &W) {
    let m = gtk::EventControllerMotion::new();
    let w2 = w.clone();
    m.connect_enter(move |_, _, _| {
        if w2.is_sensitive() {
            w2.add_css_class("pp-menu-hover");
            // Popover 的子树不会因样式类变化自动重绘（实测），显式排一次
            w2.queue_draw();
        }
    });
    let w3 = w.clone();
    m.connect_leave(move |_| {
        w3.remove_css_class("pp-menu-hover");
        w3.queue_draw();
    });
    // 弹层隐藏时指针可能没来得及 leave；清掉类，避免下次打开残留高亮
    let w4 = w.clone();
    w.connect_unmap(move |_| w4.remove_css_class("pp-menu-hover"));
    w.add_controller(m);
}

/// 开关菜单项：勾选标记在右侧，代替原生白底复选框。
pub fn toggle(icon: &str, label: &str, accel: Option<&str>, active: bool) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::new();
    b.add_css_class("pp-menu-item");
    let (r, check) = row(icon, label, accel, true);
    let Some(check) = check else {
        return b; // reserve_check 恒为真；这里只是拆 Option，正常走不到
    };
    check.set_opacity(if active { 1.0 } else { 0.0 });
    let c = check.clone();
    b.connect_toggled(move |b| c.set_opacity(if b.is_active() { 1.0 } else { 0.0 }));
    b.set_child(Some(&r));
    b.set_active(active);
    attach_hover(&b);
    b
}

/// 危险项（删除 / 关闭这类不可逆动作）：红字 + 红图标 + 淡红 hover。
pub fn danger(b: &gtk::Button) {
    b.add_css_class("pp-menu-danger");
}

/// 分组线；上下留白比原生 `Separator` 大，视觉上把菜单切成几段。
pub fn sep() -> gtk::Separator {
    let s = gtk::Separator::new(gtk::Orientation::Horizontal);
    s.add_css_class("pp-menu-sep");
    s
}

/// 菜单体到弹层**表面**的固定宽度差：`contents` 的 `padding: 6px` ×2
/// + `border: 1px` ×2 = 14px。
///
/// 用来在弹层**还没显示**（量不到表面宽度）时推出表面宽度。
///
/// ⚠️ 跟 `theme.rs` 的 `.pp-menu > contents` 一一对应：那边改了
/// `padding` / `border`，这里要同步改，否则左对齐会差几个像素。
const CARD_CHROME: f64 = 14.0;

/// GTK4 把弹层**居中**对着锚点时，用的内部 requisition 比弹层表面
/// 窄 9px（实测：表面 250 宽 → 基准 -107，而 13.5 - (250-9)/2 = -107；
/// 表面 318 宽 → 基准 -141，13.5 - (318-9)/2 = -141）。
const POPOVER_TRIM: f64 = 9.0;

/// 弹层的几何缓存。
///
/// **为什么要有**：弹层还没显示过时量不到它的位置（`translate_coordinates`
/// 要等它 map 过），而菜单一打开就要在正确的位置——不能等它先跳过去。
/// 所以先用**公式预测**基准位置，弹层显示后再用**实测值**校准。
#[derive(Clone, Copy, Default)]
struct PopGeo {
    /// 弹层**表面**原点相对按钮原点的基准位置（横向 offset = 0 时）
    base_rel_x: f64,
    /// 基准是否已经用实测值校准过（没校准就用公式预测）
    calibrated: bool,
    /// 上一次实际施加的横向偏移（校准时要从量到的位置里减掉它）
    applied_dx: i32,
}

/// 标题文字的**排版盒**左边相对按钮原点的位置。
///
/// **为什么不用 Label 控件自己的原点**：Label 是按钮的 child，它的
/// 分配区跟按钮的内容区重合，`translate_coordinates` 量出来是 0；
/// 真正决定「文字从哪儿开始」的是**按钮的内边距**
/// （`.pp-menubar-item` 的 `padding-left`）——文字的排版盒正好从
/// 按钮内容区起点开始。实测：按钮在 x=179、内边距 4 → 文字盒在 183
/// （四个菜单交叉验证过：文件/连接/终端/视图 的文字盒都正好是
/// 「按钮左 + 4」）。
fn title_rel_x(btn: &gtk::Button) -> f64 {
    btn.style_context().padding().left() as f64
}

/// 弹层表面的基准位置（相对按钮原点，横向偏移为 0 时）。
///
/// GTK4 的 `GtkPopover` 默认**居中**对着锚点按钮——`pointing_to` 只换
/// 锚点、不改对齐方式（实测：给它一个窄矩形，弹层照样以它为中心）。
/// 所以只能自己算偏移。
fn base_rel_x(btn: &gtk::Button, pop: &gtk::Popover, geo: &PopGeo) -> Option<f64> {
    if geo.calibrated {
        return Some(geo.base_rel_x);
    }
    // 预测：按钮中心 - 弹层半宽。弹层还没显示，量不到宽度，
    // 用菜单体的自然宽度 + 卡片固定差（padding + border）推出来。
    let child = pop.child()?;
    let (_, nat, _, _) = child.measure(gtk::Orientation::Horizontal, -1);
    if nat <= 0 {
        return None;
    }
    let pop_w = nat as f64 + CARD_CHROME;
    Some(btn.width() as f64 / 2.0 - (pop_w - POPOVER_TRIM) / 2.0)
}

/// 把弹层挪成「**左边缘与标题文字左边缘对齐**」（VS Code / Windows
/// 菜单栏的观感）。
///
/// 要对齐的是**卡片**（有底色那块），不是弹层表面：去掉阴影留白之后
/// 两者的左边缘重合，差的那 1px 边框由 [`CARD_CHROME`] 兜着。
fn apply_align(pop: &gtk::Popover, btn: &gtk::Button, geo: &Rc<RefCell<PopGeo>>) {
    let Some(base) = base_rel_x(btn, pop, &*geo.borrow()) else {
        return; // 量不出宽度（菜单体还没塞进去）→ 保持 GTK 默认位置
    };
    let dx = (title_rel_x(btn) - base).round() as i32;
    pop.set_offset(dx, 4);
    geo.borrow_mut().applied_dx = dx;
    if dbg_on() {
        eprintln!(
            "[菜单几何] 对齐 dx={dx} base={base:.1} 目标={:.1} btn_w={}",
            title_rel_x(btn),
            btn.width()
        );
    }
}

/// 调试开关：`PP_MENU_DEBUG=1` 时把弹层几何打到 stderr。
fn dbg_on() -> bool {
    std::env::var("PP_MENU_DEBUG").map(|v| v == "1").unwrap_or(false)
}

/// 一排「点开弹层」的菜单标题按钮（顶栏菜单栏）。
///
/// **为什么要悬停切换**：GNOME / Firefox / Windows 的菜单栏都是
/// 「打开一个菜单后，鼠标移到另一个标题上，那个菜单**自动打开**」——
/// 不用点第二下。用户 2026-10-28 明确提了这个习惯。
///
/// **实现要点**：给标题按钮挂 **Motion 控制器**，在 `enter`（鼠标移入）时
/// 切到那个菜单——原先只接了 `clicked`，压根没人监听"鼠标移过来"，
/// 于是只能点两下。
///
/// ⚠️ GTK4 的 `GtkPopover` **没有 modal 概念**（那是 GTK3 `GtkMenu` 的
/// 东西，也没有指针 grab）：弹层开着时 motion 照常派发到别的按钮上，
/// 所以悬停切换做得到。`autohide` 默认开着 → 点别处仍会自动收起。
pub struct MenuBar {
    /// 当前打开着的弹层（切菜单时先关掉它）。
    current: Rc<RefCell<Option<gtk::Popover>>>,
}

impl MenuBar {
    pub fn new() -> Self {
        MenuBar {
            current: Rc::new(RefCell::new(None)),
        }
    }

    /// 造一个菜单标题按钮，并把它的弹层挂好。
    ///
    /// 返回 `(按钮, 弹层)`：调用方往弹层里塞菜单项（用 [`item`] 等）。
    pub fn item(&self, title: &str) -> (gtk::Button, gtk::Popover) {
        let btn = gtk::Button::new();
        let lbl = gtk::Label::new(Some(title));
        btn.set_child(Some(&lbl));
        btn.add_css_class("pp-menubar-item");

        let pop = popover_detached();
        // 普通 Button 没有 popover 属性，得自己挂（MenuButton 则是它自己挂，
        // 再挂一次会报 `assertion 'parent == NULL' failed`）。
        pop.set_parent(&btn);

        // 左对齐：GTK4 的弹层默认**居中**对着标题按钮，VS Code / Windows
        // 的菜单栏是**左对齐**（弹层卡片的左边贴住标题文字的左边）。
        // 这里自己挪：公式预测基准 → 弹出前预摆位 → 显示后用实测校准。
        let geo: Rc<RefCell<PopGeo>> = Rc::new(RefCell::new(PopGeo::default()));
        {
            let btn2 = btn.clone();
            let geo2 = Rc::clone(&geo);
            pop.connect_map(move |p| {
                // 弹层刚显示：量一次**真实基准**校准公式预测值。
                // ⚠️ 实测：`translate_coordinates` 量到的是**含偏移**的
                // 位置（偏移是加在表面上的），减掉已施加的偏移才是基准。
                let p2 = p.clone();
                let b2 = btn2.clone();
                let g2 = Rc::clone(&geo2);
                glib::idle_add_local_once(move || {
                    let Some((px, _)) = p2.translate_coordinates(&b2, 0.0, 0.0) else {
                        return; // 还没定位置 → 保持公式预测值
                    };
                    {
                        let mut g = g2.borrow_mut();
                        if g.calibrated {
                            return;
                        }
                        // ⚠️ 量到的位置**含已施加的偏移**（偏移加在表面上，
                        // 控件在表面里的原点也跟着挪），要减掉才是基准。
                        g.base_rel_x = px - g.applied_dx as f64;
                        g.calibrated = true;
                    }
                    // 用实测值重算（预测不准时这里微调一下）
                    apply_align(&p2, &b2, &g2);
                });
            });
        }

        let cur = Rc::clone(&self.current);

        // 点标题：开着的就是它 → 关掉（toggle）；否则切过去。
        {
            let cur = Rc::clone(&cur);
            let pop = pop.clone();
            let btn2 = btn.clone();
            let geo2 = Rc::clone(&geo);
            btn.connect_clicked(move |_| {
                let open = cur.borrow().clone();
                if open.as_ref() == Some(&pop) {
                    pop.popdown();
                } else {
                    apply_align(&pop, &btn2, &geo2);
                    switch_to(&cur, &pop);
                }
            });
        }

        // ⚠️ **悬停切换**：鼠标移到另一个标题上，那个菜单自动打开。
        // 这是"不用点第二下"的关键。
        {
            let motion = gtk::EventControllerMotion::new();
            let cur = Rc::clone(&cur);
            let pop = pop.clone();
            let btn2 = btn.clone();
            let geo2 = Rc::clone(&geo);
            motion.connect_enter(move |_, _, _| {
                let open = cur.borrow().clone();
                if open.as_ref() == Some(&pop) {
                    return; // 本来就开着，别重复 popup（会闪一下）
                }
                apply_align(&pop, &btn2, &geo2);
                switch_to(&cur, &pop);
            });
            btn.add_controller(motion);
        }

        // 弹层自己关掉时（点了菜单项、点了外面）把"当前打开"清掉，
        // 不然下次切换会对一个已经关掉的弹层调 popdown。
        {
            let cur = Rc::clone(&cur);
            let pop_ref = pop.clone();
            pop.connect_closed(move |_| {
                let same = {
                    let b = cur.borrow();
                    b.as_ref() == Some(&pop_ref)
                };
                if same {
                    *cur.borrow_mut() = None;
                }
            });
        }

        (btn, pop)
    }

    /// 切到某个菜单：把当前开着的关掉，开这个。
    ///
    /// 悬停与点击都走它；测试也直接调它，验证"切换"这一步本身。
    pub fn switch_to(&self, pop: &gtk::Popover) {
        switch_to(&self.current, pop);
    }
}

/// 切到 `pop`：先关掉当前开着的那个，再开这个。
fn switch_to(current: &Rc<RefCell<Option<gtk::Popover>>>, pop: &gtk::Popover) {
    let open = current.borrow().clone();
    if let Some(old) = open {
        if old != *pop {
            old.popdown();
        }
    }
    pop.popup();
    *current.borrow_mut() = Some(pop.clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 没有显示服务就跳过（跟 theme.rs 的做法一致）。
    fn ensure_gtk() -> bool {
        if !gtk::is_initialized_main_thread() {
            let _ = std::panic::catch_unwind(|| gtk::init());
        }
        gtk::is_initialized_main_thread() && gtk::gdk::Display::default().is_some()
    }

    /// 菜单项必须带 `pp-menu-item` 类 **且**挂着 Motion 控制器。
    ///
    /// **为什么单测要钉这条**：Popover 里的按钮拿到 PRELIGHT 状态后，
    /// CSS `:hover` 常常只更新状态不刷背景——高亮靠 `attach_hover()`
    /// 显式加类实现。哪天有人把 `attach_hover` 删了，界面不会报错，
    /// 只是"鼠标划过去没反应"（用户 2026-10-28 报的就是这个）。
    #[test]
    fn 菜单项带类且挂_hover_控制器() {
        if !ensure_gtk() {
            eprintln!("跳过 菜单项带类且挂_hover_控制器：本机没有显示服务");
            return;
        }
        let b = item("view-list-symbolic", "查看数据", None);
        assert!(b.has_css_class("pp-menu-item"), "菜单项要带 pp-menu-item 类");
        // 图标：行里第一个子控件是 Image
        let row = b.child().expect("菜单项要有 child");
        let row = row.downcast_ref::<gtk::Box>().expect("child 是横排 Box");
        let icon = row.first_child().expect("行里要有图标");
        assert!(icon.is::<gtk::Image>(), "行里第一个控件应该是图标");
        // hover 控制器
        let model = b.observe_controllers();
        let mut has_motion = false;
        for i in 0..model.n_items() {
            if let Some(obj) = model.item(i) {
                if obj.downcast_ref::<gtk::EventControllerMotion>().is_some() {
                    has_motion = true;
                    break;
                }
            }
        }
        assert!(has_motion, "菜单项必须挂 Motion 控制器（不然划过不高亮）");
    }

    /// 菜单项文字要能读回来、也能改（凭据库菜单项要带状态）。
    #[test]
    fn 菜单项文字可读可改() {
        if !ensure_gtk() {
            eprintln!("跳过 菜单项文字可读可改：本机没有显示服务");
            return;
        }
        let b = item("dialog-password-symbolic", "凭据库…", None);
        assert_eq!(item_label(&b).as_deref(), Some("凭据库…"));
        set_item_label(&b, "凭据库…（已解锁）");
        assert_eq!(item_label(&b).as_deref(), Some("凭据库…（已解锁）"));
        // 快捷键那一列不能被当成标题改掉
        let b2 = item("edit-copy-symbolic", "复制", Some("Ctrl+Shift+C"));
        set_item_label(&b2, "复制文本");
        assert_eq!(item_label(&b2).as_deref(), Some("复制文本"));
    }

    /// `popover_detached` 不能挂父——`MenuButton::set_popover()` 自己会挂。
    ///
    /// 挂两次会报 `gtk_widget_set_parent: assertion 'parent == NULL' failed`。
    #[test]
    fn detached_弹层不挂父() {
        if !ensure_gtk() {
            eprintln!("跳过 detached_弹层不挂父：本机没有显示服务");
            return;
        }
        let pop = popover_detached();
        assert!(pop.parent().is_none(), "detached 弹层不该有父");
        assert!(pop.has_css_class("pp-menu"), "弹层要带 pp-menu 类");
    }

    /// 菜单栏标题按钮**必须挂 Motion 控制器**。
    ///
    /// **为什么单测要钉这条**：悬停切换（鼠标移到另一个菜单上，那个菜单
    /// 自动打开——GNOME / Firefox / Windows 都是这样）全靠 motion 的
    /// `enter` 事件。原先只接了 `clicked`，于是只能点两下（用户
    /// 2026-10-28 报的就是这个）。少了控制器不会报错，只是"鼠标移过去
    /// 什么都不发生"——光看代码看不出来。
    #[test]
    fn 菜单栏标题挂_hover_控制器() {
        if !ensure_gtk() {
            eprintln!("跳过 菜单栏标题挂_hover_控制器：本机没有显示服务");
            return;
        }
        let bar = MenuBar::new();
        let (btn, _pop) = bar.item("文件");
        let model = btn.observe_controllers();
        let mut has_motion = false;
        for i in 0..model.n_items() {
            if let Some(obj) = model.item(i) {
                if obj.downcast_ref::<gtk::EventControllerMotion>().is_some() {
                    has_motion = true;
                    break;
                }
            }
        }
        assert!(has_motion, "菜单栏标题必须挂 Motion 控制器（不然不能悬停切换）");
    }

    /// 切菜单时，旧的必须关上、新的必须打开。
    ///
    /// 这是悬停切换的**核心一步**（"移到另一个菜单上，那个菜单自动打开"，
    /// 而不是两个菜单叠着）。
    #[test]
    fn 菜单栏切菜单会关掉旧的() {
        if !ensure_gtk() {
            eprintln!("跳过 菜单栏切菜单会关掉旧的：本机没有显示服务");
            return;
        }
        let bar = MenuBar::new();
        let (b1, p1) = bar.item("文件");
        let (b2, p2) = bar.item("工具");
        // popover 要挂在**窗口里**才能 popup（它要 reparent 到 toplevel）
        let win = gtk::Window::new();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        row.append(&b1);
        row.append(&b2);
        win.set_child(Some(&row));
        win.present();
        let vb1 = body();
        p1.set_child(Some(&vb1));
        let vb2 = body();
        p2.set_child(Some(&vb2));

        bar.switch_to(&p1);
        assert!(p1.is_visible(), "切过去的菜单应当打开");

        bar.switch_to(&p2);
        assert!(p2.is_visible(), "新菜单应当打开");
        assert!(!p1.is_visible(), "旧菜单应当关上（不能两个叠着）");
    }
}
