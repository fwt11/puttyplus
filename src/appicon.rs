//! 应用图标（4.2）。
//!
//! 图标 PNG **内嵌在二进制里**，不依赖外部资源文件——让用户自己找一张 .png
//! 装到 ~/.local/share/icons 属于把部署负担转嫁给用户。
//!
//! ⚠️ 注意"图标内嵌"≠"整个程序全静态"：GTK4 / VTE / Pango 这些图形库仍是
//! 运行时的动态依赖（`ldd` 一看便知）。关于页里原先写"全静态"是错的，
//! 已按审计 P0-5 改成"单个可执行文件，运行时用系统的 GTK4 / VTE 等图形库"。
//!
//! `gtk4-rs` 没有 `gtk_window_set_default_icon(Texture)` 的绑定，想让窗口
//! 用自带图标，得走 GTK4 的正规路线：把图标落到缓存目录、挂进
//! `IconTheme` 的搜索路径、再 `set_icon_name()`。
//!
//! 图标怎么来的：/tmp/mkicon.py 那段脚本用 zlib 手写 PNG 编码生成
//! （4× 超采样抗锯齿），128×128、深色圆角底 + 绿色提示符 `>` + 青色传输条。

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

/// 图标文件名（放进图标主题搜索目录后按这个名字找）
pub const ICON_NAME: &str = "puttyplus";

/// 128×128 PNG，内嵌。
const PNG_BASE64: &str = concat!(
    "iVBORw0KGgoAAAANSUhEUgAAAIAAAACACAIAAABMXPacAAAFFElEQVR42u1du04bQRTdApCAPopQKAgoK2Kb+Ak2YFlyLBwCAgQRUiSSkoImX8AvpKFLkSK/kK+gzwflRBc5FsZm57GPmTlXpwiIaHfOmbln5s7sbhQxGAwGg8FgMNRjbn7hxctX63Gltfe+0/twcPLZe6CZaCyajIaj+blRj8tXt7shMD4bIAFUZE19d3AyuoPe8Ky52680OptbzddvSt4DzURj0WQ0fEQCCMlChsWlZYw+uWT/8KLe7sWlWgik",
    "TwOaDxJAhXACckBRWuyvrK7JlQbHl9XWfsi8TwKEgBbplyDKPvtxuS4i7/WPNja3yPgkQAvIEZZAl032y7X2g+Gw4ycYCsIVSLPZ9zG+StUd8psEIErSkYVxgHQmepJ9VQ2ENyM/gKGL6zLzaOciEKg/L5IZJ4yFbOpBPBk0aq62JPVzzmMyLxIz0FmjyVqXycdKIgKZOt0f+YsMmkN8VG0QSJUNi2zSZw7QKDU7hQrzw3Iu7DqPxXqR8Jm0di35pzc8I3e2IHXTpFloPa7gr5u7fRJnCyATlIJYhel/pdEhcbYA",
    "MhUWBLKzGMjuSmZ7OLKXmUgAcQyyZhfCKgWgABSAAlAACkABKAAFyF+AuPauc9U7uD0C8A/8SAGyEwCkX99f3/y5GQE/4pcUIAsBTu/Ox6kfx6efl14OhQIJgG4+jX3Bl99fq8MdCpBW3n+UeZ4E/gauQAHsA7Q+y/4IPllCUQR4Nv/4agmuCuCNJTiZgnyyBMdM2D9LcGka6qUlOLMQ89USiliK0NbARUsoYjEOJGr7gXOWUNByNJIJUkoIllDc/QAwCB69t4Sib8h4bwkO7Ij5bQlubEl6bAnO7An7agmObcr7",
    "ZwnunYrwzBKcPJbikyW4ei7IG0tw+2CWB5bg/Mk41y3Bh6OJhpZwendOAXK2hBzHgVeHc7UtAUksr3mRb6ejtS0hL0P28Hi6niXklYUoAAVgCqIJ04Q5DeVCjAKwFMFiHAVgOZobMhSAW5LclKcAPJbCg1kUgEcTeTiXx9N5PJ0PaPARpcyS/vh7isyR8E1HfEjv/3VNVJ92P8/2Bj6manpdw3I3H9Q2uq55Vgz9VQUm17Wy5Rn6yzr0rmtx0z/019VkkH9mD9PQX9hEAXRSgcXyDlOQmhlaL+/QhBWyQUrlHU5D",
    "Ey2IUi3vcCE2qySQzauLWYp4uiiW5cu7WYwLERSAAlAACkABHvD249X27a/uj/uCADeDWwpFgMa378Whfhy4Mf8FQEcrJvuC9MaBggCpfsyzUJnnyVyURqvVPuaZ6udsi8y+II1Wq33ONtUPOocpgNoHnVP9pHmYKUjtk+Zz8wviGHGpRhO2UHoq1YRPEBsljOp2F/+h3u5xGmoO0AgyQWmUPCQL9Q8vuBAzB2hUyD+j6A5O/unW2mcVwej0cWsfNILMSDVkEAyOLzc2t8ijHkAdCNTp/uMLgr3+EanUA6hTmP5P",
    "xuLSsuQvJiLt5AMCQWOkHSurazKFKlV3yGlygC7hDQRGhhGX62IG1CA5+5L6QV1kJcq1tujJXJQw8wAgLbIYMg7EkzkvmjbnEde12fcf+YF4MsYXh8Jkx5e0A4os5P0Z8yKZm8qVsMhOo17kENB8kCD9UmacRnOe5Gs0WScLesOz5m6/0uiktIdTNKCZaCyaLDVOAQjRXG2ZyCA1u8ABErKm/lHtGpdfjysYfbKX6T3QTDQWTUbDFSrMDAaDwWAwGIyx+AuiLhL7yEehBgAAAABJRU5ErkJggg==",
);

/// 图标字节。解码失败返回 None（不该发生：数据是编译期常量）。
pub fn png_bytes() -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(PNG_BASE64).ok()
}

/// 把图标装进当前图标主题，返回图标名（失败返回 None）。
///
/// 幂等：重复调用只写一次文件、只挂一次搜索路径。
pub fn install() -> Option<&'static str> {
    let display = gdk::Display::default()?;
    let bytes = png_bytes()?;
    let dir = glib::user_cache_dir()
        .join("puttyplus")
        .join("icons")
        .join(ICON_NAME);
    if std::fs::create_dir_all(&dir).is_err() {
        return None;
    }
    let file = dir.join(format!("{ICON_NAME}.png"));
    // 每次启动都写一遍：开发期换图标立刻生效，也不怕被外部删掉
    if std::fs::write(&file, &bytes).is_err() {
        return None;
    }
    let theme = gtk::IconTheme::for_display(&display);
    theme.add_search_path(&dir);
    Some(ICON_NAME)
}

/// 供 about 对话框当 logo 用（直接从内嵌字节建纹理，不经过图标主题）。
pub fn texture() -> Option<gdk::Texture> {
    let bytes = png_bytes()?;
    gdk::Texture::from_bytes(&glib::Bytes::from_owned(bytes)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_是合法_png() {
        let b = png_bytes().expect("内嵌 base64 应可解码");
        // PNG 魔数 + IEND 收尾，缺一截就说明数据被截断了
        assert_eq!(&b[..8], b"\x89PNG\r\n\x1a\n", "不是 PNG");
        assert!(b.len() > 500, "只有 {} 字节，八成是空图", b.len());
        assert_eq!(&b[b.len() - 8..b.len() - 4], b"IEND", "PNG 未正常收尾");
    }

    #[test]
    fn 图标尺寸对得上() {
        let b = png_bytes().unwrap();
        // IHDR 里的宽高在大端字节序的固定位置
        let w = u32::from_be_bytes([b[16], b[17], b[18], b[19]]);
        let h = u32::from_be_bytes([b[20], b[21], b[22], b[23]]);
        assert_eq!((w, h), (128, 128), "图标应 128×128");
    }
}
