//! PuTTYPlus 库层：除 main 入口外的全部模块（供 bin 与 examples 共用）。

pub mod a11y;
pub mod about;
pub mod active;
pub mod appicon;
pub mod args;
pub mod backend;
pub mod bridge;
pub mod config;
pub mod connmgr;
pub mod density;
pub mod emptystate;
pub mod findbar;
pub mod fwdtable;
pub mod i18n;
pub mod i18n_en;
pub mod keymgr;
pub mod keyui;
pub mod menu;
pub mod newmenu;
pub mod notify;
pub mod panel;
pub mod platform;
pub mod pwcache;
pub mod recent;
pub mod replay;
pub mod replayui;
pub mod settings;
pub mod settingsui;
pub mod shortcuts;
pub mod shortcutsui;
pub mod sidebar;
pub mod tabs;
pub mod term;
pub mod termview;
pub mod terms;
pub mod theme;
pub mod topbar;
pub mod topmenus;
pub mod util;
pub mod vault;
pub mod vaultui;
pub mod windows;

/* ---------------- 全局文案 lint（阶段 81 / 审计 P2-4、P2-8） ---------------- */

#[cfg(test)]
mod lint_tests {
    use std::path::{Path, PathBuf};

    fn src_dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
    }

    fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                rust_files(&p, out);
            } else if p.extension().map(|x| x == "rs").unwrap_or(false) {
                out.push(p);
            }
        }
    }

    /// 扫一个文件里的**用户可见字符串字面量**（跳过注释）。
    ///
    /// ⚠️ **必须处理跨行字符串**：界面上的 tooltip 大多是
    /// `"第一行\n\` 换行续写的 —— 只按行扫会把它们**整个漏掉**
    /// （回退验证时才发现：塞进去的错词一条都没报）。
    fn scan_file(src: &str, path: &str, out: &mut Vec<(String, String)>) {
        let chars: Vec<char> = src.chars().collect();
        let mut i = 0usize;
        let mut line = 1usize;
        let mut in_block = false;
        while i < chars.len() {
            let c = chars[i];
            if c == '\n' {
                line += 1;
                i += 1;
                continue;
            }
            if in_block {
                if c == '*' && i + 1 < chars.len() && chars[i + 1] == '/' {
                    in_block = false;
                    i += 2;
                    continue;
                }
                i += 1;
                continue;
            }
            // 行注释
            if c == '/' && i + 1 < chars.len() && chars[i + 1] == '/' {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            // 块注释
            if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                in_block = true;
                i += 2;
                continue;
            }
            // 字符字面量 `'"'`：里面的双引号**不是**字符串的开始
            if c == '\'' && i + 2 < chars.len() && chars[i + 1] == '"' && chars[i + 2] == '\'' {
                i += 3;
                continue;
            }
            // **原始字符串**（`r"…"` / `r#"…"#`）：本项目里只有主题的 CSS
            // 用它 ⇒ 整段跳过（CSS 不是界面文案，里面当然有 `**`）。
            if c == 'r' {
                let mut j = i + 1;
                let mut hashes = 0usize;
                while j < chars.len() && chars[j] == '#' {
                    hashes += 1;
                    j += 1;
                }
                if j < chars.len() && chars[j] == '"' {
                    // 找到结尾的 `"` + 同样多的 `#`
                    let mut k = j + 1;
                    while k < chars.len() {
                        if chars[k] == '"' {
                            let mut m = k + 1;
                            let mut h = 0usize;
                            while m < chars.len() && chars[m] == '#' && h < hashes {
                                h += 1;
                                m += 1;
                            }
                            if h == hashes {
                                i = m;
                                break;
                            }
                        }
                        k += 1;
                    }
                    if k < chars.len() {
                        continue;
                    }
                }
            }
            if c == '"' {
                let start_line = line;
                let mut s = String::new();
                i += 1;
                while i < chars.len() {
                    let d = chars[i];
                    if d == '\\' {
                        // 转义（`\` + 换行 = 续行，不算内容）
                        if i + 1 < chars.len() {
                            if chars[i + 1] == '\n' {
                                line += 1;
                            }
                            i += 2;
                            continue;
                        }
                        i += 1;
                        continue;
                    }
                    if d == '"' {
                        i += 1;
                        break;
                    }
                    if d == '\n' {
                        line += 1;
                    }
                    s.push(d);
                    i += 1;
                }
                // ⚠️ **CSS 块不算界面文案**（主题表里的整段 CSS 里面当然有
                // `**`——那是注释，不是给用户看到的字）。
                //
                // ⚠️⚠️⚠️ **判据不能用「同时含 `{` 和 `}`」**（阶段 94 实测）：
                // 那条启发式会把**又长又带 `{}` 占位符的界面文案**整个当成
                // CSS 放过 ⇒ 那段文案里的过时说法**永远查不出来**
                // （用户 2026-11-01 报「录屏的 tooltip 与现在的真实情况显然
                // 已经是不符合了」——那句过时的 tooltip 恰恰是个长
                // `format!`，所以历次 lint 一次都没扫到它）。
                //
                // 现在改成：**有「选择器 + `{`」的行**才算 CSS——本项目的 CSS
                // 全在**原始字符串**（`r#"…"#`）里，压根不会被扫到（见上面的
                // 原始字符串跳过逻辑），这条判据只是双保险。
                let is_css_block = s.len() > 200 && looks_like_css(&s);
                if !is_css_block {
                    out.push((format!("{path}:{start_line}"), s));
                }
                continue;
            }
            i += 1;
        }
    }

    /// 这段文字**像不像一段 CSS**。
    ///
    /// 判据：有一行是「**选择器 + `{`」**（去掉空白后 `{` 前面全是 ASCII）。
    /// 本项目的 CSS 全在原始字符串里，本来就扫不到；这里只是**双保险**，
    /// 别再用「含 `{` 又含 `}`」那种判据——那会把长 tooltip 放过（阶段 94）。
    fn looks_like_css(s: &str) -> bool {
        for line in s.lines() {
            let t = line.trim();
            let Some(sel) = t.strip_suffix('{') else {
                continue;
            };
            let sel = sel.trim();
            if !sel.is_empty() && sel.chars().all(|c| c.is_ascii()) {
                return true;
            }
        }
        false
    }

    /// 这个文件是不是「语法高亮的测试语料」文件（lint 要跳过）。
    ///
    /// ⚠️ **不能拿字面量 `"panel/syntax.rs"` 直接 `ends_with`**：
    ///    Windows 上路径是 `…\src\panel\syntax.rs`（**反斜杠**）⇒ 匹配不上
    ///    ⇒ 文件没被跳过 ⇒ 扫到测试语料里的 `**粗体**`
    ///    ⇒ Windows CI 的 lint 用例直接炸（第 7 轮抓到的）。
    ///
    /// ⇒ 先把反斜杠归一成正斜杠再比 —— **与运行平台无关**
    ///    ⇒ Windows 形状的路径也能在 Linux 上测（见下面的用例）。
    fn is_syntax_highlight_src(path: &str) -> bool {
        path.replace('\\', "/").ends_with("panel/syntax.rs")
    }

    /// 源码里所有**用户可见**的字符串字面量（跳过注释）。
    fn user_visible_strings() -> Vec<(String, String)> {
        let mut files = Vec::new();
        rust_files(&src_dir(), &mut files);
        let mut out = Vec::new();
        for f in files {
            // ⚠️ 语法高亮的**测试语料**里故意有 markdown 的 **粗体**
            // （那是高亮器的输入，不是界面文案）⇒ 这个文件跳过。
            if is_syntax_highlight_src(&f.to_string_lossy()) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            scan_file(&src, &f.to_string_lossy(), &mut out);
        }
        out
    }

    /// 审计 P2-4：GTK 的 tooltip / Label **不渲染 Markdown**，
    /// 写了 `**强调**` 用户看到的就是一堆星号。
    #[test]
    fn 用户可见文案里没有字面的双星号() {
        // ⚠️ 别在测试源码里直接写那两个星号（会把自己扫出来）⇒ 拼出来
        let stars: String = ['*', '*'].iter().collect();
        let bad: Vec<String> = user_visible_strings()
            .into_iter()
            .filter(|(_, s)| s.contains(&stars))
            .map(|(loc, s)| format!("{loc}: {s}"))
            .collect();
        assert!(
            bad.is_empty(),
            "界面文案里出现了字面的双星号（GTK 不渲染 Markdown）：\n{}",
            bad.join("\n")
        );
    }

    /// 跳过语法高亮的测试语料时，**Windows 形状的路径也要认**。
    ///
    /// ⚠️ 第 7 轮 Windows CI 抓到的：跳过判断拿字面量
    ///    `"panel/syntax.rs"` 去 `ends_with` ⇒ Windows 上路径是
    ///    `…\src\panel\syntax.rs` ⇒ **匹配不上** ⇒ 那个文件没被跳过
    ///    ⇒ 扫到测试语料里的 `**粗体**` ⇒ lint 用例炸。
    ///
    /// ⚠️ 这条断言在 **Linux 上就能验**（Windows 形状的路径写成字符串喂进去）
    ///    ⇒ 不用等 Windows CI 才发现。
    #[test]
    fn 跳过语法高亮语料认得反斜杠路径() {
        assert!(is_syntax_highlight_src("/a/puttyplus/src/panel/syntax.rs"));
        // Windows 上 CI 的实际路径形状
        assert!(is_syntax_highlight_src(
            r"D:\a\puttyplus\puttyplus\src\panel\syntax.rs"
        ));
        // 别的文件不能被误跳过
        assert!(!is_syntax_highlight_src("/a/puttyplus/src/panel/other.rs"));
        assert!(!is_syntax_highlight_src("/a/puttyplus/src/syntax.rs"));
    }

    /// 审计 P2-8：统一用「远程」（与「本地」成对），不用「远端」。
    #[test]
    fn 远端这个说法不用了() {
        // ⚠️ 同样别把那个词直接写出来（会把自己扫出来）⇒ 拼出来
        let bad_word: String = ['远', '端'].iter().collect();
        let bad: Vec<String> = user_visible_strings()
            .into_iter()
            .filter(|(_, s)| s.contains(&bad_word))
            .map(|(loc, s)| format!("{loc}: {s}"))
            .collect();
        assert!(
            bad.is_empty(),
            "统一用「远程」（与「本地」成对）：\n{}",
            bad.join("\n")
        );
    }

    /// 录制的说明里**不能再说过时的 asciicast 版本**（阶段 94）。
    ///
    /// 我们**写出**的是 asciicast v3（`replay.rs` 的头是 `"version": 3`），
    /// 只有**读入**才兼容 v2 ⇒ 用户可见文案里孤零零写「asciicast v2」
    /// 是错的（用户 2026-11-01 报「录屏的 tooltip 与现在的真实情况显然
    /// 已经是不符合了」）。
    #[test]
    fn 录制说明不再说过时的asciicast版本() {
        // ⚠️ 同样别把那个词直接写出来（会把自己扫出来）⇒ 拼出来
        let old_ver: String = ["asciicast ", "v2"].concat();
        let both: String = ["/ ", "v3"].concat();
        let bad: Vec<String> = user_visible_strings()
            .into_iter()
            .filter(|(_, s)| s.contains(&old_ver) && !s.contains(&both))
            .map(|(loc, s)| format!("{loc}: {s}"))
            .collect();
        assert!(
            bad.is_empty(),
            "我们写出的是 asciicast v3（只有读入兼容 v2）：\n{}",
            bad.join("\n")
        );
    }

    /// 录制的说明里**不能再声称录了输入**（阶段 94）。
    ///
    /// ⚠️ 旧文案写着「输入是编码后的按键字节」——那是阶段 87 之前的行为。
    /// 现在**输入事件根本不录**（明文密码不落盘）⇒ 照旧说会让人以为
    /// 文件里存着自己敲的密码。
    #[test]
    fn 录制说明不再声称录了输入() {
        // ⚠️ 同样别把那个词直接写出来（会把自己扫出来）⇒ 拼出来
        let bad_words: Vec<String> = [
            ["输入是编码后的", "按键字节"].concat(),
            ["录到的", "按键"].concat(),
        ]
        .into_iter()
        .collect();
        let bad: Vec<String> = user_visible_strings()
            .into_iter()
            .filter(|(_, s)| bad_words.iter().any(|w| s.contains(w)))
            .map(|(loc, s)| format!("{loc}: {s}"))
            .collect();
        assert!(
            bad.is_empty(),
            "我们不录输入事件（明文密码不落盘），文案别这么说：\n{}",
            bad.join("\n")
        );
    }

    /// 复查 N1：页签「可重排」的调用**必须在挂页之后**。
    ///
    /// GTK 要求页**先在 notebook 里**才谈得上可重排；反过来 GTK 报
    /// `gtk_notebook_set_tab_reorderable: assertion 'list != NULL' failed`
    /// 并**静默丢弃**这个标记 ⇒ **所有页签都拖不动**，而且界面不报错。
    ///
    /// ⚠️ GTK4 **没有**读回"这一页可不可重排"的接口 ⇒ 只能断言**源码顺序**
    /// （这类"静默无效"本来就得靠源码/几何断言钉住）。
    #[test]
    fn 页签可重排要在挂页之后() {
        let p = src_dir().join("tabs.rs");
        let src = std::fs::read_to_string(&p).expect("读 tabs.rs");
        let lines: Vec<&str> = src.lines().collect();
        let set_at = lines
            .iter()
            .position(|l| l.contains("set_tab_reorderable(&page_w"))
            .expect("tabs.rs 里应当有 set_tab_reorderable(&page_w …)");
        let app_at = lines
            .iter()
            .position(|l| l.contains("append_page(&page_w"))
            .expect("tabs.rs 里应当有 append_page(&page_w …)");
        assert!(
            app_at < set_at,
            "set_tab_reorderable 必须在 append_page 之后\
             （tabs.rs:{} 在 tabs.rs:{} 之前）\
             ⇒ 顺序反了 GTK 会静默丢弃标记，页签拖不动",
            set_at + 1,
            app_at + 1
        );
    }

    /// ⚠️ **回归钉**：凭据库状态标志**只许建一处**（阶段 92）。
    ///
    /// 用户 2026-11-01 报「界面上有两处『凭证库 已锁定』是否重复」——
    /// 顶栏（`main.rs`）与**侧栏标题栏**（`sidebar.rs`）各建了一个徽标，
    /// 同一个状态在屏幕上出现两次。
    ///
    /// 留的是**侧栏那个**（阶段 77a/77b 的既定设计：那儿是配连接、勾
    /// 「记住密码」的地方）；顶栏那个已删。
    #[test]
    fn 凭据库标志只建一处() {
        // ⚠️ 名字**拼出来**：直接写会把自己扫出来
        let ctor = format!("{}badge_button(", "vaultui::");
        let mut files = Vec::new();
        rust_files(&src_dir(), &mut files);
        let mut hits: Vec<String> = Vec::new();
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            for (i, line) in src.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                if line.contains(&ctor) {
                    hits.push(format!("{}:{}: {}", f.display(), i + 1, line.trim()));
                }
            }
        }
        assert_eq!(
            hits.len(),
            1,
            "凭据库状态标志只许建一处（现在建了 {} 处）：\n{}\n\
             顶栏那个已删（阶段 92）——同一个状态显示两次就是重复。",
            hits.len(),
            hits.join("\n")
        );
    }

    /// ⚠️ **回归钉**：顶栏**不再有「＋新建」按钮**（阶段 92c）。
    ///
    /// 用户 2026-11-01：「顶栏的加号也可以去掉了」——同一个「新建」入口出现
    /// 两遍（侧栏标题栏已经有一个，菜单项与顶栏那份**完全相同**：
    /// `NewEntry::items()` 对 TopBar / Sidebar 都给全集）⇒ 重复。
    ///
    /// 顶栏那个删了之后，"新建"只剩侧栏那一个 ⇒ 这条防它又长回来。
    #[test]
    fn 顶栏不再建新建按钮() {
        let src = std::fs::read_to_string(src_dir().join("main.rs")).expect("读 main.rs");
        // ⚠️ 名字**拼出来**：直接写会把自己扫出来
        let needle = format!("{}TopBar", "NewEntry::");
        let hits: Vec<String> = src
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("//") && l.contains(&needle)
            })
            .map(|l| l.trim().to_string())
            .collect();
        assert!(
            hits.is_empty(),
            "顶栏不该再建「＋新建」按钮（侧栏已经有一个，菜单项完全相同）：\n{}",
            hits.join("\n")
        );
    }

    /// 审计 P2-7：图标按钮**统一**走 `a11y::icon_button`。    ///
    /// 绕过去建按钮 ⇒ 可访问名称没人管 ⇒ 读屏只会念"按钮"。
    /// （`a11y.rs` 里那个 helper 自己当然要用底下的构造器。）
    #[test]
    fn 图标按钮统一走可访问构件() {
        let mut files = Vec::new();
        rust_files(&src_dir(), &mut files);
        let mut bad = Vec::new();
        // ⚠ 构造器名字**拼出来**：直接写会把这条 lint 自己扫出来
        let ctor = format!("{}from_icon_name(", "gtk::Button::");
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            let mut line_no = 0usize;
            for line in src.lines() {
                line_no += 1;
                // 只看代码行：注释里提到构造器不算
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with("///") || t.starts_with("*") {
                    continue;
                }
                if line.contains(&ctor) && !f.ends_with("a11y.rs") {
                    bad.push(format!(
                        "{}:{}: {}",
                        f.display(),
                        line_no,
                        line.trim()
                    ));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "图标按钮要走 `a11y::icon_button`（否则没有可访问名称）：\n{}",
            bad.join("\n")
        );
    }

    /// 审计 P2-5：对话框按钮词表统一。
    ///
    /// 同一个动作只能有一个说法：确定 / 取消 / 关闭 / 保存。
    /// 英文按钮（`gtk::ButtonsType::OkCancel` 之类）会**按系统语言翻译**——
    /// 没装中文翻译的机器上就冒出 "OK / Cancel"，跟全中文界面割裂。
    #[test]
    fn 对话框按钮词表统一() {
        // 只看**按钮位**（`("字", gtk::ResponseType::…)`），
        // 免得误伤命令行自测输出里的 "OK" 之类
        let banned = [
            "(\"OK\"",
            "(\"Cancel\"",
            "(\"Close\"",
            "(\"Yes\"",
            "(\"No\"",
            "(\"好\"",
        ];
        let mut files = Vec::new();
        rust_files(&src_dir(), &mut files);
        let mut bad = Vec::new();
        for f in files {
            let Ok(src) = std::fs::read_to_string(&f) else {
                continue;
            };
            for (i, line) in src.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                if banned.iter().any(|b| line.contains(b)) {
                    bad.push(format!("{}:{}: {}", f.display(), i + 1, line.trim()));
                }
            }
        }
        assert!(
            bad.is_empty(),
            "对话框按钮只用「确定 / 取消 / 关闭 / 保存」（英文会跟着系统语言跑）：\n{}",
            bad.join("\n")
        );
    }
}
