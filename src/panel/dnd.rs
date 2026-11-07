//! 拖放接收器（阶段 100）。
//!
//! ## 为什么不用 `gtk::DropTarget`（同步版）
//!
//! GTK 的 `GtkDropTarget` 在「拖拽进入」时把 `GdkDrop` 存进 `self->drop`，
//! **只有真正落下（`drop` 事件）才会被清掉**。而它读数据是**异步**的
//! （`gtk_drop_target_load()` → `gdk_drop_read_value_async`）：
//!
//! ```c
//! case GDK_DROP_START:
//!   self->dropping = TRUE;
//!   if (gtk_drop_target_load (self))   /* 读数据还没回来 ⇒ 返回 FALSE */
//!     gtk_drop_target_do_drop (self);
//!   return TRUE;
//! ```
//!
//! ⇒ 如果这次异步读**一直没回调**（源端不应答、或用户中途松手），
//! `self->dropping` 永远为 TRUE、`self->drop` 永远留着旧 drop。而 GTK 在
//! 下一次拖拽进入时的判断是：
//!
//! ```c
//! if (self->drop != NULL)
//!   return;            /* 已经有 drop 了 ⇒ 这次的新 drop 直接被忽略 */
//! ```
//!
//! ⇒ **一次「没落下的拖拽」会让这个控件上所有的后续拖拽全部失效**：
//! `self->drop` 不更新，事件里的 drop 对不上，GTK 报
//! `gtk_drop_target_handle_event: assertion 'self->drop == drop' failed`
//! 并**直接丢掉事件** —— 用户看到的就是「拖进来完全没反应」。
//!
//! `GtkDropTargetAsync` 没有这个问题：它在拖拽**离开**时清掉
//! `self->drop`，而且没有 `dropping` 这个中间状态。
//!
//! ## 顺带解决的三件事
//!
//! 1. **自己按 mime 判定接不接**，不再依赖 GTK 那张「mime → GType」的
//!    映射表。同步版只认 GType，而同进程内「只给 mime 的源」与
//!    「要 GType 的目标」是**匹配不上**的 —— 这就是「远程列表 → 本地列表」
//!    的下载拖拽一直协商不成功的根因。
//! 2. **读数据自己选类型**：本地文件读 `text/uri-list`（跨进程通用，
//!    文件管理器、浏览器都提供），远端路径行读 `G_TYPE_STRING`。
//! 3. **读数据加超时**：源端不应答时不能永远挂着（见 `READ_TIMEOUT`）。

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gtk::gdk;
use gtk::prelude::*;

/// X11 选区直读（阶段 101）—— 只有 X11/Unix 才有这个协议。
#[cfg(unix)]
use super::xdnd11;

/// 远程路径行用的自定义 mime（拖拽源用）。
pub const REMOTE_MIME: &str = "application/x-puttyplus-remote";

/// 本地文件列表用的标准 mime。
pub const URI_LIST_MIME: &str = "text/uri-list";

/// 纯文本（远程路径行也按这个再给一份，兼容只认 text/plain 的目标）。
pub const TEXT_MIME: &str = "text/plain;charset=utf-8";

/// 读数据的超时（秒）。
///
/// **为什么必须有**：X11 上源端可以「接了拖拽却never应答选区转换」，
/// GTK 的选区输入流**没有超时**，会一直挂着 —— 用户看到的就是「拖进来
/// 完全没反应」（和拖拽没触发一模一样）。超时要明确告诉用户「对方没给
/// 数据」，而不是让界面永远停在那里。
pub const READ_TIMEOUT_SECS: u64 = 5;

/// 拖放要什么。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Want {
    /// 本地文件（拖到远程列表 / 终端 ⇒ 上传）。
    Files,
    /// 远端路径行（拖到本地列表 ⇒ 下载）。
    Paths,
}

/// 一次拖放的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DropResult {
    /// 拿到了路径（上传=本地路径，下载=远端绝对路径）。
    Paths(Vec<String>),
    /// 源端没给出数据（拒绝、超时、或给的格式里没有可用路径）。
    NoData,
}

/// `GdkFileList` → 本地路径列表。
///
/// ⚠️ `GFile::path()` 拿不到本地路径的文件（非 `file://` 的 URI）会被丢掉
/// —— 但**不能静默**：调用方拿到的空列表要有话说（见 `drop_upload_paths`）。
pub fn filelist_paths(fl: &gdk::FileList) -> Vec<String> {
    fl.files()
        .iter()
        .filter_map(|f| f.path().map(|p| p.to_string_lossy().into_owned()))
        .filter(|p| !p.is_empty())
        .collect()
}

/// 远端路径行文本 → 路径列表。
///
/// 一行一个绝对路径；空行、`..` 之类的相对路径都跳过。
pub fn parse_paths(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter(|l| l.starts_with('/'))
        .map(str::to_string)
        .collect()
}

/// `text/uri-list` 文本 → 本地路径列表。
///
/// 一行一个 URI（`file:///…`）；`#` 开头是注释行（RFC 2483）。
///
/// ⚠️ 百分号编码交给 `gio::File::for_uri` 解（`a%20b.txt` → `a b.txt`）；
/// 带主机名的 `file://host/path` 拿不到本地路径，跳过。
pub fn parse_uri_list(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let path = if line.starts_with("file://") {
            match gtk::gio::File::for_uri(line).path() {
                Some(p) => p.to_string_lossy().into_owned(),
                None => continue,
            }
        } else {
            line.to_string()
        };
        if !path.starts_with('/') {
            continue;
        }
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// 这次拖拽提供的东西里，能不能满足 `want`。
pub fn formats_satisfy(formats: &gdk::ContentFormats, want: Want) -> bool {
    match want {
        Want::Files => {
            formats.contains_type(gdk::FileList::static_type())
                || formats.contain_mime_type(URI_LIST_MIME)
        }
        Want::Paths => {
            formats.contains_type(glib::Type::STRING)
                || formats.contain_mime_type(TEXT_MIME)
                || formats.contain_mime_type("text/plain")
                || formats.contain_mime_type(REMOTE_MIME)
        }
    }
}

/// 把一个流读完（最多 1 MiB，够放一万条路径了）。
pub fn slurp(stream: &gtk::gio::InputStream, cb: impl FnOnce(String) + 'static) {
    use gtk::gio::prelude::InputStreamExt;
    stream.read_bytes_async(
        1024 * 1024,
        glib::Priority::DEFAULT,
        None::<&gtk::gio::Cancellable>,
        move |r| match r {
            Ok(b) => cb(String::from_utf8_lossy(&b).into_owned()),
            Err(_) => cb(String::new()),
        },
    );
}

/// 给控件挂一个拖放接收器。
///
/// `on_result` 收到拖放的结果（拿到路径 / 源端没给数据）。
///
/// ⚠️ 回调在**异步读取完成之后**才跑（GTK 的数据是异步取的），
/// 所以它运行在主循环里，但已经不是 drop 事件的同步上下文了。
pub fn attach<F>(w: &gtk::Widget, want: Want, on_result: F)
where
    F: Fn(DropResult) + 'static,
{
    // formats = None ⇒ 先全接，再由 accept 回调按实际内容决定。
    let dst = gtk::DropTargetAsync::new(None, gdk::DragAction::COPY);
    let on_result = Rc::new(on_result);

    // 弱引用拿回控件：X11 直读要知道「收到拖放的窗口」是谁（落点窗口）。
    //
    // ⚠️ 抓**强引用**就是「控件 → 控制器 → 闭包 → 控件」的**引用环**，
    // 控件永远释放不掉（AGENTS.md 的红线）。
    let weak_widget: glib::WeakRef<gtk::Widget> = {
        let mut wr = glib::WeakRef::new();
        wr.set(Some(w));
        wr
    };

    dst.connect_accept(move |_d, drop| formats_satisfy(&drop.formats(), want));

    // 告诉源「我接受复制」——不接的话源会当场把拖拽撤了。
    dst.connect_drag_enter(move |_d, _drop, _x, _y| gdk::DragAction::COPY);
    dst.connect_drag_motion(move |_d, _drop, _x, _y| gdk::DragAction::COPY);

    dst.connect_drop(move |_d, drop, _x, _y| {
        // drop 是借用，异步回调里还要用 ⇒ 先 clone 一份（它是 GObject）。
        let drop_read = drop.clone();
        let drop_finish = drop.clone();
        let on_result = Rc::clone(&on_result);
        // 「读完没有」：读回调与超时回调**谁先到谁算数**，`finish()` 只能调一次。
        let done = Rc::new(Cell::new(false));

        // 超时兜底：源端不应答时（Deepin 文件管理器就是这样）不能永远挂着。
        {
            let done = Rc::clone(&done);
            let drop_finish = drop_finish.clone();
            let on_result = Rc::clone(&on_result);
            glib::timeout_add_local_once(Duration::from_secs(READ_TIMEOUT_SECS), move || {
                if done.replace(true) {
                    return; // 已经读到了
                }
                on_result(DropResult::NoData);
                drop_finish.finish(gdk::DragAction::COPY);
            });
        }

        match want {
            Want::Files => {
                // ★ 先走 **X11 直读**（见 `xdnd11` 的说明）：GDK4 的读法用
                //   「隐藏 leader 窗口 + CurrentTime」去问源端，老式源端
                //   （Deepin 文件管理器 = Qt5）会**直接拒绝** ⇒ 一个字节都
                //   拿不到。自己按 XDND 惯例再问一次就能拿到。
                //
                // 只在「确实有跨进程拖拽」时才会真等（程序内部拖拽时选区没有
                // 持有者，`xdnd11` 会立刻返回 `None`），不会拖慢内部拖拽。
                //
                // ⚠️ **X11 专有**：Windows/macOS 没有选区转换协议 ⇒ 这一步
                //    整个不存在，直接走下面的 `text/uri-list` 标准读法。
                #[cfg(unix)]
                if let Some(text) = weak_widget
                    .upgrade()
                    .and_then(|w| xdnd11::read_uri_list(&w))
                {
                    let paths = parse_uri_list(&text);
                    if !paths.is_empty() && !done.replace(true) {
                        on_result(DropResult::Paths(paths));
                        drop_finish.finish(gdk::DragAction::COPY);
                        return true;
                    }
                }
                // 本地文件：读原始的 `text/uri-list`。
                //
                // ⚠️ 不用 `GdkFileList` 类型：那个类型靠 GDK 的反序列化表
                // 转换，跨进程时能不能对上取决于 GDK 版本；原始 mime 是
                // 所有文件管理器/浏览器都提供的标准做法。
                let mimes: &[&str] = &[URI_LIST_MIME];
                drop_read.read_async(
                    mimes,
                    glib::Priority::DEFAULT,
                    None::<&gtk::gio::Cancellable>,
                    move |r| match r {
                        Ok((stream, _mime)) => {
                            let done = Rc::clone(&done);
                            let drop_finish = drop_finish.clone();
                            let on_result = Rc::clone(&on_result);
                            slurp(&stream, move |text| {
                                if done.replace(true) {
                                    return;
                                }
                                let paths = parse_uri_list(&text);
                                if paths.is_empty() {
                                    on_result(DropResult::NoData);
                                } else {
                                    on_result(DropResult::Paths(paths));
                                }
                                drop_finish.finish(gdk::DragAction::COPY);
                            });
                        }
                        Err(_) => {
                            if done.replace(true) {
                                return;
                            }
                            on_result(DropResult::NoData);
                            drop_finish.finish(gdk::DragAction::COPY);
                        }
                    },
                );
            }
            Want::Paths => {
                // 远端路径行：读成字符串（源给的是一行一个绝对路径）。
                drop_read.read_value_async(
                    glib::Type::STRING,
                    glib::Priority::DEFAULT,
                    None::<&gtk::gio::Cancellable>,
                    move |r| {
                        if done.replace(true) {
                            return;
                        }
                        let paths = match r {
                            Ok(v) => v
                                .get::<String>()
                                .map(|s| parse_paths(&s))
                                .unwrap_or_default(),
                            Err(_) => Vec::new(),
                        };
                        if paths.is_empty() {
                            on_result(DropResult::NoData);
                        } else {
                            on_result(DropResult::Paths(paths));
                        }
                        drop_finish.finish(gdk::DragAction::COPY);
                    },
                );
            }
        }
        true
    });

    w.add_controller(dst);
}

/**
 * 拖拽**源**要提供的内容。
 *
 * ## 为什么给「类型化 + 原始 mime」两份
 *
 * - **类型化**（`GdkFileList` / `G_TYPE_STRING`）：**同进程内**拖拽
 *   （本程序自己的面板之间）时，目标按 GType 读，一步到位。
 * - **原始 mime**（`text/uri-list` / `text/plain`）：**跨进程**拖拽
 *   （别的程序拖进来）时，X11 选择机制只能传 mime 字符串。
 *
 * 只给前者 ⇒ 跨进程拖不进来；只给后者 ⇒ 同进程内 GTK 匹配不上
 * （实测：`text/plain` 的字节源与 `G_TYPE_STRING` 目标**连 accept 都不触发**）。
 */
pub fn source_provider(typed: glib::Value, mime: &str, body: &str) -> gdk::ContentProvider {
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&typed),
        gdk::ContentProvider::for_bytes(mime, &glib::Bytes::from(body.as_bytes())),
    ])
}

/// 本地文件列表 → 拖拽源（上传方向）。
pub fn files_provider(paths: &[String]) -> gdk::ContentProvider {
    use glib::prelude::ToValue;
    let files: Vec<gtk::gio::File> = paths.iter().map(|p| gtk::gio::File::for_path(p)).collect();
    let fl = gdk::FileList::from_array(&files);
    let body = paths
        .iter()
        .map(|p| format!("file://{p}\r\n"))
        .collect::<String>();
    source_provider(fl.to_value(), URI_LIST_MIME, &body)
}

/// 远端路径行 → 拖拽源（下载方向）。
pub fn paths_provider(paths: &[String]) -> gdk::ContentProvider {
    use glib::prelude::ToValue;
    let body = paths.join("\n");
    let text = body.clone();
    gdk::ContentProvider::new_union(&[
        gdk::ContentProvider::for_value(&text.to_value()),
        gdk::ContentProvider::for_bytes(REMOTE_MIME, &glib::Bytes::from(body.as_bytes())),
        gdk::ContentProvider::for_bytes(
            TEXT_MIME,
            &glib::Bytes::from(paths.join("\n").as_bytes()),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析远端路径行() {
        assert_eq!(
            parse_paths("/srv/a.txt\n/srv/b.txt\n"),
            vec!["/srv/a.txt".to_string(), "/srv/b.txt".to_string()]
        );
        // 相对路径、空行都要跳过
        assert_eq!(parse_paths("\n  \nrel/path\n/x\n"), vec!["/x".to_string()]);
    }

    #[test]
    fn 解析_uri列表() {
        assert_eq!(
            parse_uri_list("file:///tmp/a.txt\r\nfile:///tmp/b.txt\r\n"),
            vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()]
        );
        // 注释行、空行要跳过；重复的只留一个
        assert_eq!(
            parse_uri_list("# 这是注释\n\nfile:///tmp/a.txt\nfile:///tmp/a.txt\n"),
            vec!["/tmp/a.txt".to_string()]
        );
        // 百分号编码也要能解开
        assert_eq!(
            parse_uri_list("file:///tmp/a%20b.txt\n"),
            vec!["/tmp/a b.txt".to_string()]
        );
    }

    #[test]
    fn 文件列表取本地路径() {
        let files = vec![
            gtk::gio::File::for_path("/tmp/a.txt"),
            gtk::gio::File::for_path("/tmp/b.txt"),
        ];
        let fl = gdk::FileList::from_array(&files);
        assert_eq!(
            filelist_paths(&fl),
            vec!["/tmp/a.txt".to_string(), "/tmp/b.txt".to_string()]
        );
    }

    #[test]
    fn 格式判定_文件方向() {
        let f = gdk::ContentFormats::for_type(gdk::FileList::static_type());
        assert!(formats_satisfy(&f, Want::Files));
        let f = gdk::ContentFormats::new(&[URI_LIST_MIME]);
        assert!(formats_satisfy(&f, Want::Files));
        // 纯文本不该被当成文件
        let f = gdk::ContentFormats::new(&[TEXT_MIME]);
        assert!(!formats_satisfy(&f, Want::Files));
        assert!(formats_satisfy(&f, Want::Paths));
    }

    #[test]
    fn 格式判定_路径方向() {
        let f = gdk::ContentFormats::for_type(glib::Type::STRING);
        assert!(formats_satisfy(&f, Want::Paths));
        let f = gdk::ContentFormats::new(&[REMOTE_MIME]);
        assert!(formats_satisfy(&f, Want::Paths));
    }
}
