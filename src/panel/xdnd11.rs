//! X11 选区直读（阶段 101）。
//!
//! ## 为什么需要它
//!
//! GDK4 读拖拽数据的唯一入口是 `gdk_drop_read_async()`，它在 X11 后端是：
//!
//! ```c
//! gdk_x11_drop_read_async (…)
//!   → gdk_x11_selection_input_stream_new_async (display, "XdndSelection", target,
//!                                             CurrentTime, …)
//!   → XConvertSelection (display, "XdndSelection", target, prop,
//!                         GDK_X11_DISPLAY (display)->leader_window,  ← 请求方窗口
//!                         CurrentTime);                               ← 时间戳
//! ```
//!
//! 这两个参数都不合 XDND 惯例：
//!
//! - **请求方窗口**是 GDK 的隐藏 leader 窗口 —— 它**没有 `XdndAware` 属性**；
//! - **时间戳**是 `CurrentTime`，不是 XdndDrop 事件里带的那个。
//!
//! 源端收到 `SelectionRequest` 后要**自己判断**「这个请求配不配得上我这次
//! 拖拽」。Deepin 文件管理器是 Qt5 写的，`QXcbDrag::handleSelectionRequest()`：
//!
//! ```c
//! if (currentDrag () && event->time == source_time)          at = -2;  /* 时间戳对得上 */
//! else if ((at = findTransactionByTime (event->time)) == -1)          /* 事务时间戳对得上 */
//!      if ((at = findTransactionByWindow (event->requestor)) == -1)   /* 请求方就是落点窗口 */
//!          if ((target = findXdndAwareParent (…, event->requestor)))  /* 请求方能上溯到 XdndAware */
//!              if (event->time == XCB_CURRENT_TIME && current_target == target)
//!                  at = -2;
//! ```
//!
//! ⇒ GDK4 那条路**三条都走不通**：leader 窗口没有 `XdndAware`、上溯到 root
//! 也没有；`CurrentTime` 对不上任何事务。源端于是回一个
//! `SelectionNotify` 且 `property = None` ⇒ **不供数**。
//! 这就是「从文件管理器拖进 PuTTYPlus 没反应」的根因。
//!
//! GTK3 为什么行（xterminal 就是 GTK3）：`gtk_drag_get_data()` 用的是
//! **拖放事件的时间戳**（`gtk/gtkdnd.c`：
//! `gtk_selection_convert (ipc_widget, "XdndSelection", target, time_)`），
//! 正好命中 Qt 的 `findTransactionByTime()`。
//!
//! ## 我们怎么做
//!
//! 自己发一次 `XConvertSelection`，参数按 XDND 惯例来：
//!
//! - **请求方 = 收到拖放的那个顶层窗口**（GDK4 把 `XdndAware` 挂在顶层
//!   surface 上，见 `gdk/x11/gdksurface-x11.c` 的
//!   `_gdk_x11_surface_register_dnd()`）⇒ 命中源端的 `findXdndAwareParent()`
//!   分支。实测：带 `XdndAware` 的窗口 + `CurrentTime` 就能拿到数据。
//! - 时间戳用 `CurrentTime`（GDK4 不把 XdndDrop 的时间戳暴露给应用，
//!   拿不到；靠请求方窗口就够用了）。
//!
//! 源端把数据写进**请求方窗口的属性**里，我们轮询这个属性即可 ——
//! **不必去接 `SelectionNotify` 事件**（那个事件会发给请求方窗口的拥有者，
//! 正好是我们自己，但 GDK 会先把它读走；直接读属性更稳）。
//!
//! ## 安全边界
//!
//! - **只在 X11 后端生效**：Wayland 走 GDK 原生通道，`gdk_x11_*` 那套没有意义
//!   （按 display 名字判断，直接返回 `None`，走原来的 GDK 读法）。
//! - **读不出来就返回 `None`**：调用方照旧走 GDK 的读法（同进程拖拽、
//!   GTK4 写的源端都走那条），行为不倒退。
//! - **不跨进程抢事件**：全程只用 `XConvertSelection` / `XGetWindowProperty`，
//!   **绝不调 `XNextEvent`** —— 那会把 GDK 的事件抢走。

use std::ffi::{c_char, c_int, c_uchar, c_long, c_ulong, c_void};
use std::ptr;
use std::slice;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::glib::translate::ToGlibPtr;
use gtk::prelude::*;

/// X11 的 `CurrentTime`（= 0）。
const CURRENT_TIME: c_ulong = 0;
/// `AnyPropertyType`（= 0）。
const ANY_PROPERTY_TYPE: c_ulong = 0;
/// `XGetWindowProperty` 的成功返回码。
const X_SUCCESS: c_int = 0;

/// 等源端应答的最长时间（毫秒）。
///
/// 源端是**另一个进程**，应答是毫秒级的（实测 < 10ms）。
/// 这里给 1.2 秒是「源端刚好在忙」的余量；再长就会让界面卡住。
const WAIT_MS: u64 = 1200;

/// 每次请求用独一无二的属性名（同名会读到上一次的残留）。
static SEQ: AtomicU32 = AtomicU32::new(0);

#[link(name = "X11")]
unsafe extern "C" {
    fn XInternAtom(display: *mut c_void, name: *const c_char, only_if_exists: c_int) -> c_ulong;
    fn XGetSelectionOwner(display: *mut c_void, selection: c_ulong) -> c_ulong;
    fn XConvertSelection(
        display: *mut c_void,
        selection: c_ulong,
        target: c_ulong,
        property: c_ulong,
        requestor: c_ulong,
        time: c_ulong,
    ) -> c_int;
    fn XGetWindowProperty(
        display: *mut c_void,
        window: c_ulong,
        property: c_ulong,
        long_offset: c_long,
        long_length: c_long,
        delete: c_int,
        req_type: c_ulong,
        actual_type: *mut c_ulong,
        actual_format: *mut c_int,
        nitems: *mut c_ulong,
        bytes_after: *mut c_ulong,
        prop_return: *mut *mut c_uchar,
    ) -> c_int;
    fn XDeleteProperty(display: *mut c_void, window: c_ulong, property: c_ulong) -> c_int;
    fn XFree(data: *mut c_void) -> c_int;
    fn XFlush(display: *mut c_void) -> c_int;
}

unsafe extern "C" {
    /// GDK 的 X11 后端函数（Debian 把 GDK4 并进了 `libgtk-4.so.1`）。
    fn gdk_x11_display_get_xdisplay(display: *mut gdk::ffi::GdkDisplay) -> *mut c_void;
    fn gdk_x11_surface_get_xid(surface: *mut gdk::ffi::GdkSurface) -> c_ulong;
}

/// 向源端要一份 `text/uri-list`（X11 专用）。
///
/// 返回 `None` = 这条路走不通（非 X11 后端 / 不是跨进程拖拽 / 源端不应答），
/// 调用方照旧走 GDK 的读法。
pub fn read_uri_list(widget: &gtk::Widget) -> Option<String> {
    unsafe { read_mime(widget, "text/uri-list") }
}

/// 向源端要一份指定 mime 的原始文本（X11 专用）。
pub fn read_mime(widget: &gtk::Widget, mime: &str) -> Option<String> {
    unsafe {
        // 顶层窗口 = 收到拖放的那个窗口（`XdndAware` 就挂在它的 surface 上）。
        let surface = widget.root()?.native()?.surface()?;
        let display = surface.display();
        // 只在 X11 后端做：Wayland 的 display 名字是 `wayland-0`，X11 是 `:0`。
        if !display.name().starts_with(':') {
            return None;
        }
        let xdisplay = gdk_x11_display_get_xdisplay(display.to_glib_none().0);
        if xdisplay.is_null() {
            return None;
        }
        let requestor = gdk_x11_surface_get_xid(surface.to_glib_none().0);
        if requestor == 0 {
            return None;
        }
        read_selection(xdisplay, requestor, mime)
    }
}

/// 发一次选区转换请求，然后轮询属性等源端把数据写进来。
unsafe fn read_selection(xdisplay: *mut c_void, requestor: c_ulong, mime: &str) -> Option<String> {
    let selection = intern(xdisplay, "XdndSelection")?;
    // 没有跨进程拖拽在进行（程序内部拖拽就是这样）⇒ 不用白等。
    if XGetSelectionOwner(xdisplay, selection) == 0 {
        return None;
    }
    let target = intern(xdisplay, mime)?;
    let prop = intern(
        xdisplay,
        &format!(
            "PP_DND_{}_{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ),
    )?;

    // 请求方 = 落点窗口（带 XdndAware）⇒ 命中源端的 findXdndAwareParent 分支。
    XConvertSelection(xdisplay, selection, target, prop, requestor, CURRENT_TIME);
    XFlush(xdisplay);

    let deadline = Instant::now() + Duration::from_millis(WAIT_MS);
    loop {
        let mut actual_type: c_ulong = 0;
        let mut actual_format: c_int = 0;
        let mut nitems: c_ulong = 0;
        let mut bytes_after: c_ulong = 0;
        let mut data: *mut c_uchar = ptr::null_mut();
        let rc = XGetWindowProperty(
            xdisplay,
            requestor,
            prop,
            0,
            65536,
            0,
            ANY_PROPERTY_TYPE,
            &mut actual_type,
            &mut actual_format,
            &mut nitems,
            &mut bytes_after,
            &mut data,
        );
        if rc == X_SUCCESS && actual_type != 0 {
            let out = if actual_format == 8 && !data.is_null() {
                let bytes = slice::from_raw_parts(data, nitems as usize);
                Some(String::from_utf8_lossy(bytes).into_owned())
            } else {
                None
            };
            if !data.is_null() {
                XFree(data as *mut c_void);
            }
            XDeleteProperty(xdisplay, requestor, prop);
            XFlush(xdisplay);
            return out;
        }
        if !data.is_null() {
            XFree(data as *mut c_void);
        }
        if Instant::now() >= deadline {
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    // 超时：把占位属性清掉，别留给下一次。
    XDeleteProperty(xdisplay, requestor, prop);
    XFlush(xdisplay);
    None
}

/// 内部化一个原子名。
unsafe fn intern(xdisplay: *mut c_void, name: &str) -> Option<c_ulong> {
    let mut buf = Vec::with_capacity(name.len() + 1);
    buf.extend_from_slice(name.as_bytes());
    buf.push(0);
    let atom = XInternAtom(xdisplay, buf.as_ptr() as *const c_char, 0);
    if atom == 0 { None } else { Some(atom) }
}
