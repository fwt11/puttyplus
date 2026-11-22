//! 跨格子拖拽的**负载协议**（阶段 145）。
//!
//! **统一成一种 mime ＋ 一种负载格式**：
//!
//! - mime：`application/x-pp-page`
//! - 负载：`<kind>:<id>`（`terminal:7` / `db:3` / `docker:2` / `editor:42`）
//!
//! ⚠️ **为什么不用 `text/plain`**：文件面板的拖放也用字符串（远端路径行）
//!    ⇒ 混用的话，拖编辑器标签到文件列表上会被当成"拖路径"⇒ 误触发下载。
//!
//! ⚠️ **为什么负载里要带种类**：同一个 id 在不同种类里含义不同
//!    （子页签 3 与编辑器 3 是两回事）⇒ 只给 id 的话，落点分不清。
//!
//! ⚠️ **为什么页签 id 要全局唯一**（不是每个 `SubTabs` 自己从 1 数）：
//!    跨格子搬页签时，落点只拿到**页签 id** ⇒ 得能反查「它在哪个格子」
//!    ⇒ 每个 `SubTabs` 各自编号的话，两个格子都可能有 id = 3 ⇒ 说不清。

use std::sync::atomic::{AtomicU64, Ordering};

/// 跨格子拖拽用的自定义 mime（阶段 145）。
pub const PAGE_MIME: &str = "application/x-pp-page";

/// 全局页签编号计数器（**所有 `SubTabs` 共用**）。
pub static NEXT_PAGE_ID: AtomicU64 = AtomicU64::new(1);

/// 分配一个新的**全局唯一**页签 id。
pub fn alloc_page_id() -> u64 {
    NEXT_PAGE_ID.fetch_add(1, Ordering::Relaxed)
}

/// 拖拽负载的**种类**。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKindTag {
    /// 终端子页签。
    Terminal,
    /// 数据库工作台子页签。
    Db,
    /// 容器子页签。
    Docker,
    /// 编辑器（阶段的 137 三态：主栏 / 底部 / 侧栏）。
    Editor,
    /// **文件面板页**（阶段 146：文件面板独立成格之后，它是一格里的**一页**）。
    Files,
    /// 其它（不认识就按"其它"处理，别丢数据）。
    Other,
}

impl PageKindTag {
    /// 这个种类在负载里的写法（`terminal` / `db` / `docker` / `editor` / `other`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            PageKindTag::Terminal => "terminal",
            PageKindTag::Db => "db",
            PageKindTag::Docker => "docker",
            PageKindTag::Editor => "editor",
            PageKindTag::Files => "files",
            PageKindTag::Other => "other",
        }
    }

    /// 从负载里的写法认种类（认不出来 ⇒ `None`）。
    pub fn from_str(s: &str) -> Option<PageKindTag> {
        match s.trim().to_ascii_lowercase().as_str() {
            "terminal" | "term" => Some(PageKindTag::Terminal),
            "db" | "database" => Some(PageKindTag::Db),
            "docker" | "container" => Some(PageKindTag::Docker),
            "editor" | "edit" => Some(PageKindTag::Editor),
            "files" | "file" => Some(PageKindTag::Files),
            "other" => Some(PageKindTag::Other),
            _ => None,
        }
    }
}

/// 编码拖拽负载（`<kind>:<id>`）。
pub fn encode_payload(kind: PageKindTag, id: u64) -> String {
    format!("{}:{}", kind.as_str(), id)
}

/// 解码拖拽负载 ⇒ (种类, id)。
///
/// ⚠️ **兼容三种写法**（前两种是旧格式，阶段 108 之前就在用）：
/// - `editor:42` —— 新格式；
/// - `pp-editor:42` —— 旧的编辑器负载（前缀带 `pp-`）；
/// - `42` —— 裸 id（也当编辑器，历史上只有编辑器这么拖过）。
pub fn parse_page_payload(s: &str) -> Option<(PageKindTag, u64)> {
    let t = s.trim();
    if t.is_empty() {
        return None;
    }
    // 旧前缀：`pp-editor:42`
    if let Some(rest) = t.strip_prefix("pp-editor:") {
        let id = rest.trim().parse::<u64>().ok()?;
        return Some((PageKindTag::Editor, id));
    }
    match t.split_once(':') {
        Some((k, v)) => {
            let kind = PageKindTag::from_str(k)?;
            let id = v.trim().parse::<u64>().ok()?;
            Some((kind, id))
        }
        // 裸 id（历史格式）
        None => {
            let id = t.parse::<u64>().ok()?;
            Some((PageKindTag::Editor, id))
        }
    }
}

/// 拖到格子**边缘** ⇒ 拆分语义（往哪边拆）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitZone {
    /// 左边 ⇒ 拆成「左格 | 原格」。
    Left,
    /// 右边 ⇒ 拆成「原格 | 右格」。
    Right,
    /// 上边 ⇒ 拆成「上格 / 原格」（上下叠放）。
    Top,
    /// 下边 ⇒ 拆成「原格 / 下格」（上下叠放）。
    Bottom,
}

impl SplitZone {
    /// 拆分方向：`true` = 上下叠放（竖向 Paned），`false` = 左右并排。
    pub fn stacked(&self) -> bool {
        matches!(self, SplitZone::Top | SplitZone::Bottom)
    }

    /// **新格子放前面吗**（左 / 上 ⇒ 是）。
    pub fn new_first(&self) -> bool {
        matches!(self, SplitZone::Left | SplitZone::Top)
    }
}

/// 边缘判定：离哪条边最近就往哪边拆。
///
/// ⚠️ **只在边缘 `margin` 像素内才算拆分落点** ⇒ 格子中间是「不接受」
///    （不然用户往中间一放，格子就被劈开了）。
///
/// ⚠️ **量不出尺寸时返回 `None`**（还没布局完 ⇒ 别乱拆）。
pub fn zone_at(x: f64, y: f64, w: i32, h: i32, margin: f64) -> Option<SplitZone> {
    if w <= 0 || h <= 0 {
        return None;
    }
    let (w, h) = (w as f64, h as f64);
    let m = margin.min(w / 2.0).min(h / 2.0);
    if m <= 0.0 {
        return None;
    }
    let mut best: Option<(f64, SplitZone)> = None;
    for (d, z) in [
        (x, SplitZone::Left),
        (w - x, SplitZone::Right),
        (y, SplitZone::Top),
        (h - y, SplitZone::Bottom),
    ] {
        if d < 0.0 || d > m {
            continue;
        }
        if best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, z));
        }
    }
    best.map(|(_, z)| z)
}

/// 插入位置：把落点 x 换算成**第几个页签之前**。
///
/// ⚠️ **纯函数**：页签头的 x 坐标由调用方给（GTK 没有公开页签栏 API ⇒
///    我们自己记页签头 widget 的位置）。
pub fn insert_index_at(xs: &[f64], x: f64) -> usize {
    for (i, hx) in xs.iter().enumerate() {
        if x < *hx {
            return i;
        }
    }
    xs.len()
}

/// 跨格子的**动作**（阶段 145）：落点要干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageMove {
    /// 搬进**这个格子**的页签栏（插到第几个之前；`None` = 追加到最后）。
    Into(Option<usize>),
    /// 拆到**这个格子**的某条边上（本格子被劈成两半，页签进新的一半）。
    Split(SplitZone),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 负载往返() {
        for (kind, id) in [
            (PageKindTag::Terminal, 7u64),
            (PageKindTag::Db, 3),
            (PageKindTag::Docker, 2),
            (PageKindTag::Editor, 42),
            (PageKindTag::Other, 0),
        ] {
            let s = encode_payload(kind, id);
            assert_eq!(parse_page_payload(&s), Some((kind, id)), "{s}");
        }
    }

    #[test]
    fn 负载格式就是种类冒号id() {
        assert_eq!(encode_payload(PageKindTag::Terminal, 7), "terminal:7");
        assert_eq!(encode_payload(PageKindTag::Db, 3), "db:3");
        assert_eq!(encode_payload(PageKindTag::Docker, 2), "docker:2");
        assert_eq!(encode_payload(PageKindTag::Editor, 42), "editor:42");
    }

    #[test]
    fn 旧格式也能解析() {
        // 阶段 108 的旧负载
        assert_eq!(
            parse_page_payload("pp-editor:12"),
            Some((PageKindTag::Editor, 12))
        );
        // 裸 id（更老的写法）
        assert_eq!(parse_page_payload("42"), Some((PageKindTag::Editor, 42)));
        // 前后空白不算错（拖放数据偶尔带换行）
        assert_eq!(
            parse_page_payload(" terminal:7\n"),
            Some((PageKindTag::Terminal, 7))
        );
    }

    #[test]
    fn 不是这个格式就解不出() {
        assert_eq!(parse_page_payload(""), None);
        assert_eq!(parse_page_payload("/srv/a.txt"), None);
        assert_eq!(parse_page_payload("terminal:"), None);
        assert_eq!(parse_page_payload("terminal:abc"), None);
        // 前缀对但后面是别的 ⇒ 不能瞎解析
        assert_eq!(parse_page_payload("editor:12"), Some((PageKindTag::Editor, 12)));
        assert_eq!(parse_page_payload("nope:12"), None);
    }

    #[test]
    fn 种类名认得几个别名() {
        assert_eq!(PageKindTag::from_str("terminal"), Some(PageKindTag::Terminal));
        assert_eq!(PageKindTag::from_str("TERM"), Some(PageKindTag::Terminal));
        assert_eq!(PageKindTag::from_str("database"), Some(PageKindTag::Db));
        assert_eq!(PageKindTag::from_str("container"), Some(PageKindTag::Docker));
        assert_eq!(PageKindTag::from_str("nope"), None);
    }

    #[test]
    fn 页签id全局唯一() {
        let a = alloc_page_id();
        let b = alloc_page_id();
        assert_ne!(a, b);
        assert!(b > a);
    }

    #[test]
    fn 边缘判定认四条边() {
        // 400×300 的格子，边缘 24px
        assert_eq!(zone_at(5.0, 150.0, 400, 300, 24.0), Some(SplitZone::Left));
        assert_eq!(zone_at(395.0, 150.0, 400, 300, 24.0), Some(SplitZone::Right));
        assert_eq!(zone_at(200.0, 5.0, 400, 300, 24.0), Some(SplitZone::Top));
        assert_eq!(zone_at(200.0, 295.0, 400, 300, 24.0), Some(SplitZone::Bottom));
        // 中间 ⇒ 不算落点
        assert_eq!(zone_at(200.0, 150.0, 400, 300, 24.0), None);
    }

    #[test]
    fn 量不出尺寸就不判边缘() {
        assert_eq!(zone_at(0.0, 0.0, 0, 300, 24.0), None);
        assert_eq!(zone_at(0.0, 0.0, 400, 0, 24.0), None);
        // ⚠️ **边缘宽度会夹到格子的一半**（不会超过）⇒ 极小的格子里
        //    整块都算边缘（这种格子本来就没法再拆了）。
        assert_eq!(zone_at(1.0, 150.0, 40, 300, 24.0), Some(SplitZone::Left));
        // 正常的格子：中间不是边缘。
        assert_eq!(zone_at(200.0, 150.0, 400, 300, 24.0), None);
    }

    #[test]
    fn 拆分方向对() {
        assert!(!SplitZone::Left.stacked());
        assert!(!SplitZone::Right.stacked());
        assert!(SplitZone::Top.stacked());
        assert!(SplitZone::Bottom.stacked());
        // 左 / 上 ⇒ 新格子在前面
        assert!(SplitZone::Left.new_first());
        assert!(SplitZone::Top.new_first());
        assert!(!SplitZone::Right.new_first());
        assert!(!SplitZone::Bottom.new_first());
    }

    #[test]
    fn 插入位置算得对() {
        // 三个页签头分别在 x = 10 / 60 / 110
        let xs = [10.0, 60.0, 110.0];
        assert_eq!(insert_index_at(&xs, 5.0), 0);
        assert_eq!(insert_index_at(&xs, 30.0), 1);
        assert_eq!(insert_index_at(&xs, 70.0), 2);
        assert_eq!(insert_index_at(&xs, 200.0), 3);
        // 一个页签都没有 ⇒ 插到最前
        assert_eq!(insert_index_at(&[], 10.0), 0);
    }
}
