//! 从**剪贴板**取本地文件（阶段 100）。
//!
//! ## 为什么要有这条路径
//!
//! 拖拽上传（`super::dnd`）在 X11 上依赖源程序提供「选区转换」数据。
//! 绝大多数程序（GTK 程序、浏览器、Nautilus）都提供，Deepin 文件管理器
//! （dde-file-manager / Qt）**也提供** —— 但它对「怎么问」很挑：
//! 请求的**时间戳**必须对得上那次拖拽，或者**请求方窗口**必须是收到
//! 拖放的那个窗口，否则一律回「不支持」（`property=None`）。
//! 阶段 101 用 `super::xdnd11` 按这个规矩自己问一次就拿到了（见那里的说明）。
//!
//! 即便如此，剪贴板这条路仍然值得留着：
//!
//! - **不依赖拖拽**：有些环境（Wayland、远程桌面）拖拽根本走不到；
//! - **一次能带多个文件**，比逐个拖方便；
//! - 源端出问题时（换了别的文件管理器）还有兜底。
//!
//! 而同一个文件管理器的**剪贴板是好的**：复制文件后剪贴板里有
//! `text/uri-list`（`file:///…`），也有纯文本路径。
//!
//! ⇒ 所以给用户一条**一定能用**的路：在文件管理器里 `Ctrl+C` 复制，
//! 在这里「粘贴上传」。

use std::cell::RefCell;
use std::rc::Rc;

/// 剪贴板（系统剪贴板）。
///
/// ⚠️ 跨平台版统一走 `platform::clipboard`（阶段 C0）——
/// GTK4 的剪贴板本身跨平台，这里只是收口。
fn clipboard() -> Option<gtk::gdk::Clipboard> {
    crate::platform::clipboard::clipboard()
}

/// 剪贴板文本 → 本地路径列表。
///
/// **只保留确实存在的文件/目录**：剪贴板里也可能是普通文字（用户刚复制
/// 了一段话），这时不该把碰巧长得像路径的行当成文件。
pub fn local_paths_from_text(text: &str) -> Vec<String> {
    super::dnd::parse_uri_list(text)
        .into_iter()
        .filter(|p| std::path::Path::new(p).exists())
        .collect()
}

/// 读剪贴板里的本地文件路径（异步）。
///
/// 先读 `text/uri-list`（标准做法）；拿不到再退回纯文本 —— Deepin 文件
/// 管理器的 `text/plain` 里直接就是路径。
pub fn read_local_paths<F>(cb: F)
where
    F: Fn(Vec<String>) + 'static,
{
    let Some(clip) = clipboard() else {
        cb(Vec::new());
        return;
    };
    // 回调只能被调用一次 ⇒ 用 Option 包一层
    let slot = Rc::new(RefCell::new(Some(cb)));

    let clip2 = clip.clone();
    let slot2 = Rc::clone(&slot);
    clip.read_async(
        &[super::dnd::URI_LIST_MIME],
        glib::Priority::DEFAULT,
        None::<&gtk::gio::Cancellable>,
        move |r| match r {
            Ok((stream, _mime)) => {
                let slot = Rc::clone(&slot2);
                super::dnd::slurp(&stream, move |text| {
                    let paths = local_paths_from_text(&text);
                    if let Some(f) = slot.borrow_mut().take() {
                        f(paths);
                    }
                });
            }
            Err(_) => {
                // 退回纯文本
                let slot = Rc::clone(&slot2);
                clip2.read_text_async(None::<&gtk::gio::Cancellable>, move |r2| {
                    let text = r2
                        .ok()
                        .flatten()
                        .map(|s| s.to_string())
                        .unwrap_or_default();
                    let paths = local_paths_from_text(&text);
                    if let Some(f) = slot.borrow_mut().take() {
                        f(paths);
                    }
                });
            }
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 剪贴板文本只留真存在的文件() {
        let real = crate::util::testutil::temp_dir("pp_clip_test", "text").join("clip_test.txt");
        std::fs::write(&real, b"x").unwrap();
        let text = format!(
            "{}\n/tmp/绝对不存在/xxx.txt\n随便一段文字\n",
            real.to_string_lossy()
        );
        let paths = local_paths_from_text(&text);
        assert_eq!(paths, vec![real.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_file(&real);
    }

    #[test]
    fn 剪贴板_uri列表能解出路径() {
        let real = crate::util::testutil::temp_dir("pp_clip_test", "uri").join("clip_test2.txt");
        std::fs::write(&real, b"x").unwrap();
        let text = format!("file://{}\n", real.to_string_lossy());
        let paths = local_paths_from_text(&text);
        assert_eq!(paths, vec![real.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_file(&real);
    }
}
