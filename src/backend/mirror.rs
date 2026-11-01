//! 单向目录镜像（6.2）的**计划**部分：扫目录、比对、定动作。
//!
//! 方向是**本地 → 远端**，就这一条。双向同步是经典陷阱：本地删了文件，
//! 远端也删吗？远端被人改了呢？谁说了算？没有唯一正确答案的功能，
//! 宁可不做——所以这里没有"远端 → 本地"，也不做冲突合并。
//!
//! 这一层刻意只做"算"，不碰网络也不碰 GTK，所以整份判定逻辑都能单测；
//! 真正的建目录/上传/删除由 `panel/mirrorui.rs` 排好序后丢进现成的
//! 传输队列执行。
//!
//! ## 判定规则（预览里逐条显示，别让用户猜）
//!
//! | 情况 | 动作 |
//! |---|---|
//! | 只在本地有（目录） | 建目录 |
//! | 只在本地有（文件） | 上传 |
//! | 两边都有（都是目录） | 无变化 |
//! | 两边都有（都是文件，大小不同） | 上传 |
//! | 两边都有（都是文件，大小相同、本地更新） | 上传 |
//! | 两边都有（都是文件，大小相同、远端更新或一样） | 无变化 |
//! | 一边目录一边文件 | **冲突，不动**（不猜意图，列出来让人决定） |
//! | 只在远端有 | 默认不动；勾选"删除远端多出的文件"后才列为待删 |
//!
//! 上传后会把远端 mtime 对齐到本地（`keep_mtime`），所以**第二次扫描
//! 一定是"无变化"**——不然服务器时钟一偏，每次扫都会把同样的文件重传一遍，
//! 那就不是镜像而是永动机了。

use std::collections::BTreeMap;
use std::path::Path;

use super::conn::SftpBackend;

/// 递归深度上限（与传输侧一致）。超了记成冲突，不静默截断。
pub const MAX_DEPTH: usize = 32;

/// mtime 容差（秒）。
///
/// 只用于"大小相同、靠时间判断"的场景。留 2 秒是为了吸收时间戳粒度
/// 与轻微时钟偏移；真正的判据仍然是**大小优先**。
pub const MTIME_SLACK: i64 = 2;

/// 内置忽略规则。空字符串=什么都不忽略。
pub const DEFAULT_IGNORE: &str =
    ".git,.svn,.hg,node_modules,__pycache__,target,dist,.DS_Store,*.swp,*~";

/* ==================================================================
 * 忽略规则
 * ================================================================== */

/// 按名字匹配的忽略列表（rsync `--exclude` 的简化版：只看条目名，
/// 不看路径、不支持 `**` 与 `!` 取反）。
///
/// 目录命中就整棵子树跳过——镜像一个带 `.git` 的工作目录会把几万个
/// 对象全传上去，这是最该挡掉的一种。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Ignore {
    pats: Vec<String>,
}

impl Ignore {
    /// 逗号/空白/换行分隔都可以，方便手写。
    pub fn parse(s: &str) -> Ignore {
        Ignore {
            pats: s
                .split([',', ' ', '\t', '\n', '\r'])
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(str::to_string)
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.pats.is_empty()
    }

    pub fn pats(&self) -> &[String] {
        &self.pats
    }

    /// 条目名是否命中。命中就整条（目录则整棵子树）跳过。
    pub fn hit(&self, name: &str) -> bool {
        self.pats.iter().any(|p| glob_match(p, name))
    }
}

/// `*`（任意长）、`?`（单字符）通配，其余按字面比。双指针 + 回溯。
pub fn glob_match(pat: &str, s: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = s.chars().collect();
    let (mut i, mut j) = (0usize, 0usize);
    let mut star: Option<usize> = None;
    let mut mark = 0usize;
    while j < t.len() {
        if i < p.len() && (p[i] == '?' || p[i] == t[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == '*' {
            star = Some(i);
            mark = j;
            i += 1;
        } else if let Some(si) = star {
            i = si + 1;
            mark += 1;
            j = mark;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == '*' {
        i += 1;
    }
    i == p.len()
}

/* ==================================================================
 * 扫描结果
 * ================================================================== */

/// 一个文件或目录（相对根的路径用 `/`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub rel: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix 秒；0 = 拿不到
    pub mtime: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    pub entries: Vec<Entry>,
    /// 被忽略规则或软链跳过的条目（预览里如实列出来，不静默吞）
    pub skipped: Vec<String>,
    /// 根目录本身是否存在（远端不存在时要把根也建出来）
    pub root_exists: bool,
    /// 超过 MAX_DEPTH 被拦下的路径
    pub too_deep: Vec<String>,
}

/// 本地扫描。整块在 worker 线程上跑，别卡 UI。
pub fn walk_local(root: &Path, ig: &Ignore) -> Result<Scan, String> {
    let mut scan = Scan {
        root_exists: true,
        ..Default::default()
    };
    if !root.is_dir() {
        return Err(format!("本地目录 {} 不存在或不是目录", root.display()));
    }
    let mut stack: Vec<(std::path::PathBuf, String, usize)> =
        vec![(root.to_path_buf(), String::new(), 0)];
    while let Some((dir, rel, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            scan.too_deep.push(rel);
            continue;
        }
        let rd = std::fs::read_dir(&dir)
            .map_err(|e| format!("打开本地目录 {} 失败：{e}", dir.display()))?;
        for e in rd {
            let e = e.map_err(|x| format!("读取 {} 失败：{x}", dir.display()))?;
            let name = e.file_name().to_string_lossy().into_owned();
            if ig.hit(&name) {
                scan.skipped.push(join(&rel, &name));
                continue;
            }
            let md = match e.metadata() {
                Ok(m) => m,
                Err(x) => {
                    scan.skipped.push(format!("{}（读不到属性：{x}）", join(&rel, &name)));
                    continue;
                }
            };
            // 软链一律不跟、不传：跟进去可能绕成环，指到远端的软链更是另一套语义
            if md.file_type().is_symlink() {
                scan.skipped.push(format!("{}（软链接）", join(&rel, &name)));
                continue;
            }
            let r = join(&rel, &name);
            let is_dir = md.is_dir();
            scan.entries.push(Entry {
                rel: r.clone(),
                is_dir,
                size: if is_dir { 0 } else { md.len() },
                mtime: md
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0),
            });
            if is_dir {
                stack.push((e.path(), r, depth + 1));
            }
        }
    }
    sort_entries(&mut scan.entries);
    Ok(scan)
}

fn join(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

fn sort_entries(v: &mut [Entry]) {
    v.sort_by(|a, b| a.rel.cmp(&b.rel));
}

/* ==================================================================
 * 计划
 * ================================================================== */

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Mkdir,
    Upload,
    Delete,
    /// 一边目录一边文件：不动，列给人看
    Conflict,
    Same,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Action::Mkdir => "建目录",
            Action::Upload => "上传",
            Action::Delete => "删除",
            Action::Conflict => "冲突（不动）",
            Action::Same => "无变化",
        }
    }

    /// 要不要执行。冲突与无变化都不动。
    pub fn acts(self) -> bool {
        matches!(self, Action::Mkdir | Action::Upload | Action::Delete)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub action: Action,
    pub rel: String,
    pub size: u64,
    pub local_mtime: i64,
    pub remote_mtime: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Opts {
    /// 把"只在远端有"的列为待删。**默认关**——删除必须由用户显式点头。
    pub delete: bool,
    pub mtime_slack: i64,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            delete: false,
            mtime_slack: MTIME_SLACK,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub items: Vec<Item>,
    pub n_mkdir: usize,
    pub n_upload: usize,
    pub n_delete: usize,
    pub n_same: usize,
    pub n_conflict: usize,
    pub upload_bytes: u64,
    /// 只在远端有、但没开删除的（预览里单独交代一句）
    pub remote_only: Vec<String>,
}

impl Plan {
    /// 预览里要列出来的项（默认不含"无变化"）。
    pub fn visible(&self, show_same: bool) -> Vec<&Item> {
        self.items
            .iter()
            .filter(|i| show_same || i.action != Action::Same)
            .collect()
    }

    pub fn acts(&self) -> usize {
        self.n_mkdir + self.n_upload + self.n_delete
    }

    pub fn is_empty(&self) -> bool {
        self.acts() == 0
    }

    /// 一句话摘要（预览顶部）。
    pub fn summary(&self, root_missing: bool) -> String {
        let mut s = String::new();
        if root_missing {
            s.push_str("远程目录不存在，将新建；");
        }
        s.push_str(&format!(
            "上传 {} 个（{}）、建目录 {} 个、删除 {} 个",
            self.n_upload,
            crate::util::human_size(self.upload_bytes),
            self.n_mkdir,
            self.n_delete
        ));
        if !self.remote_only.is_empty() {
            s.push_str(&format!("；远程另有 {} 个本地没有的（未勾选删除，不动）", self.remote_only.len()));
        }
        if self.n_conflict > 0 {
            s.push_str(&format!("；{} 项类型冲突，不动", self.n_conflict));
        }
        s
    }
}

/// 两份扫描结果 → 执行计划。
///
/// **`root_missing` = 远端根目录不存在**（注意是"不存在"，不是"存在"；
/// 传反了会让计划里凭空多一个建目录项）。传 true 时会把远端根本身
/// 也列成待建，否则下面的相对路径全都建不出来。
pub fn build_plan(local: &[Entry], remote: &[Entry], o: &Opts, root_missing: bool) -> Plan {
    let lm: BTreeMap<&str, &Entry> = local.iter().map(|e| (e.rel.as_str(), e)).collect();
    let rm: BTreeMap<&str, &Entry> = remote.iter().map(|e| (e.rel.as_str(), e)).collect();

    let mut p = Plan::default();
    let mut items: Vec<Item> = Vec::new();

    for (rel, le) in &lm {
        let item = match rm.get(rel) {
            None => Item {
                action: if le.is_dir { Action::Mkdir } else { Action::Upload },
                rel: (*rel).to_string(),
                size: le.size,
                local_mtime: le.mtime,
                remote_mtime: 0,
            },
            Some(re) => {
                let action = if le.is_dir && re.is_dir {
                    Action::Same
                } else if le.is_dir != re.is_dir {
                    // 类型对不上：不动。一边目录一边文件多半是人为的，
                    // 猜错方向就是毁数据
                    Action::Conflict
                } else if le.size != re.size {
                    Action::Upload
                } else if le.mtime > re.mtime + o.mtime_slack {
                    Action::Upload
                } else {
                    Action::Same
                };
                Item {
                    action,
                    rel: (*rel).to_string(),
                    size: le.size,
                    local_mtime: le.mtime,
                    remote_mtime: re.mtime,
                }
            }
        };
        match item.action {
            Action::Mkdir => p.n_mkdir += 1,
            Action::Upload => {
                p.n_upload += 1;
                p.upload_bytes += item.size;
            }
            Action::Same => p.n_same += 1,
            Action::Conflict => p.n_conflict += 1,
            _ => {}
        }
        items.push(item);
    }

    for (rel, _re) in &rm {
        if lm.contains_key(rel) {
            continue;
        }
        if o.delete {
            p.n_delete += 1;
            items.push(Item {
                action: Action::Delete,
                rel: (*rel).to_string(),
                size: 0,
                local_mtime: 0,
                remote_mtime: 0,
            });
        } else {
            p.remote_only.push((*rel).to_string());
        }
    }

    if root_missing {
        // 根目录本身：远程要新建，否则下面的相对路径全都建不出来
        p.n_mkdir += 1;
        items.push(Item {
            action: Action::Mkdir,
            rel: String::new(),
            size: 0,
            local_mtime: 0,
            remote_mtime: 0,
        });
    }

    items.sort_by(|a, b| a.rel.cmp(&b.rel));
    p.items = items;
    p
}

fn depth(rel: &str) -> usize {
    if rel.is_empty() {
        0
    } else {
        rel.matches('/').count() + 1
    }
}

/// 执行顺序：**先建目录（浅→深）→ 再上传 → 最后删（深→浅）**。
///
/// 顺序不能乱：子目录要先有，删除必须最后——传一半失败时把远端删空了，
/// 比少传几个文件糟得多。
pub fn order<'a>(p: &'a Plan) -> Vec<&'a Item> {
    let pick = |a: Action| -> Vec<&Item> { p.items.iter().filter(|i| i.action == a).collect() };
    let mut mk = pick(Action::Mkdir);
    mk.sort_by_key(|i| depth(&i.rel));
    let mut up = pick(Action::Upload);
    up.sort_by(|a, b| a.rel.cmp(&b.rel));
    let mut del = pick(Action::Delete);
    // 深→浅：先删文件再删它所在的目录（remove_rec 自己也会递归，
    // 但排好序的预览读起来才不费劲）
    del.sort_by(|a, b| depth(&b.rel).cmp(&depth(&a.rel)));
    let mut out = Vec::with_capacity(mk.len() + up.len() + del.len());
    out.append(&mut mk);
    out.append(&mut up);
    out.append(&mut del);
    out
}

/// 远端绝对路径 = 根 + 相对路径。根以 `/` 结尾的斜杠要拼对。
pub fn remote_path(root: &str, rel: &str) -> String {
    let r = root.trim_end_matches('/');
    if rel.is_empty() {
        if r.is_empty() { "/".to_string() } else { r.to_string() }
    } else if r.is_empty() {
        format!("/{rel}")
    } else {
        format!("{r}/{rel}")
    }
}

/// 本地绝对路径 = 根 + 相对路径。
pub fn local_path(root: &Path, rel: &str) -> std::path::PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/* ==================================================================
 * 远端那一侧
 * ================================================================== */

impl SftpBackend {
    /// 递归扫描远端目录。根不存在时返回 `root_exists=false` 的空结果
    /// （不是错误——那正好是要新建的情形）。
    pub async fn walk_dir(&self, root: &str, ig: &Ignore) -> Result<Scan, String> {
        let mut scan = Scan::default();
        if !self.is_dir(root).await {
            scan.root_exists = false;
            return Ok(scan);
        }
        scan.root_exists = true;
        let base = root.trim_end_matches('/').to_string();
        let mut stack: Vec<(String, String, usize)> = vec![(base, String::new(), 0)];
        while let Some((abs, rel, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                scan.too_deep.push(rel);
                continue;
            }
            let items = self.list(&abs).await.map_err(|e| e.0)?;
            for it in items {
                let r = join(&rel, &it.name);
                if ig.hit(&it.name) {
                    scan.skipped.push(r);
                    continue;
                }
                // 软链接不跟：本地侧也跳，两边才对称
                if it.is_link {
                    scan.skipped.push(format!("{r}（软链接）"));
                    continue;
                }
                scan.entries.push(Entry {
                    rel: r.clone(),
                    is_dir: it.is_dir,
                    size: it.size,
                    mtime: it.mtime,
                });
                if it.is_dir {
                    let sub = if abs.is_empty() {
                        it.name.clone()
                    } else {
                        format!("{abs}/{}", it.name)
                    };
                    stack.push((sub, r, depth + 1));
                }
            }
        }
        sort_entries(&mut scan.entries);
        Ok(scan)
    }

    /// 镜像上传：传完把远端 mtime 对齐到本地。
    ///
    /// 不对齐的话，服务器时钟偏一点就会让**每次扫描都把这些文件重传一遍**
    /// ——那不叫镜像，叫永动机。对齐失败（服务器不给 setstat 权限）不
    /// 算传输失败，只在附注里说明。
    pub async fn mirror_upload(
        &self,
        lpath: &str,
        rpath: &str,
        progress: &super::sftp::ProgressSink,
        note: &super::sftp::XferNote,
    ) -> super::BackendResult<bool> {
        self.upload_file(lpath, rpath, progress, note).await?;
        let t = std::fs::metadata(lpath)
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as u32);
        if let Some(t) = t {
            let attrs = russh_sftp::protocol::FileAttributes {
                mtime: Some(t),
                ..Default::default()
            };
            if self.sftp().set_metadata(rpath, attrs).await.is_err() {
                note.append("远程时间未对齐（服务器不允许改 mtime）");
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(rel: &str, size: u64, mtime: i64) -> Entry {
        Entry {
            rel: rel.into(),
            is_dir: false,
            size,
            mtime,
        }
    }
    fn d(rel: &str, mtime: i64) -> Entry {
        Entry {
            rel: rel.into(),
            is_dir: true,
            size: 0,
            mtime,
        }
    }
    fn acts(p: &Plan) -> Vec<(Action, &str)> {
        order(p).into_iter().map(|i| (i.action, i.rel.as_str())).collect()
    }

    /* ---- 通配 ---- */

    #[test]
    fn 通配_字面与星问号() {
        assert!(glob_match("a.txt", "a.txt"));
        assert!(!glob_match("a.txt", "a.txtx"));
        assert!(glob_match("*.swp", ".file.swp"));
        assert!(glob_match("*.swp", ".swp"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match("*", ""));
        assert!(glob_match("a*", "abc"));
        // "a*b" 要求以 b 结尾：abc 不匹配
        assert!(!glob_match("a*b", "abc"));
        assert!(glob_match("a*b", "ab"));
        assert!(glob_match("*.swp", "x.swp"));
        assert!(!glob_match("a*b", "a"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match(".git", ".git"));
        assert!(!glob_match(".git", ".github"));
    }

    #[test]
    fn 忽略_逗号空白换行都认() {
        let ig = Ignore::parse(" .git , node_modules\n*~ ");
        assert_eq!(ig.pats().len(), 3);
        assert!(ig.hit(".git"));
        assert!(ig.hit("node_modules"));
        assert!(ig.hit("a.txt~"));
        assert!(!ig.hit("main.rs"));
    }

    #[test]
    fn 忽略_空串等于什么都不忽略() {
        assert!(Ignore::parse("  ").is_empty());
        assert!(!Ignore::parse("  ").hit(".git"));
    }

    /* ---- 本地扫描 ---- */

    #[test]
    fn 扫描_递归且忽略命中目录整棵跳过() {
        let base = std::env::temp_dir().join("pp_mirror_scan");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("sub/deep")).unwrap();
        std::fs::create_dir_all(base.join("skipme/inside")).unwrap();
        std::fs::write(base.join("a.txt"), b"12345").unwrap();
        std::fs::write(base.join("sub/b.txt"), b"1").unwrap();
        std::fs::write(base.join("sub/deep/c.txt"), b"12").unwrap();
        std::fs::write(base.join("skipme/inside/x.txt"), b"nope").unwrap();

        let sc = walk_local(&base, &Ignore::parse(".git,skipme")).unwrap();
        let rels: Vec<&str> = sc.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, ["a.txt", "sub", "sub/b.txt", "sub/deep", "sub/deep/c.txt"]);
        assert_eq!(sc.entries[0].size, 5);
        assert!(sc.skipped.iter().any(|s| s == "skipme"), "跳过的应如实报出：{:?}", sc.skipped);
        assert!(!rels.iter().any(|r| r.starts_with("skipme")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn 扫描_根不存在给明确错误() {
        let e = walk_local(std::path::Path::new("/definitely/not/here"), &Ignore::default())
            .unwrap_err();
        assert!(e.contains("不存在"), "{e}");
    }

    /* ---- 计划 ---- */

    #[test]
    fn 计划_只在本地有的建目录并上传() {
        let p = build_plan(
            &[d("x", 1), f("x/a", 10, 100)],
            &[],
            &Opts::default(),
            false,
        );
        assert_eq!(acts(&p), [(Action::Mkdir, "x"), (Action::Upload, "x/a")]);
        assert_eq!(p.upload_bytes, 10);
    }

    #[test]
    fn 计划_大小不同就传_相同且不新就跳过() {
        let o = Opts::default();
        // 大小不同 → 传（哪怕远端更新）
        let p = build_plan(&[f("a", 5, 100)], &[f("a", 6, 200)], &o, false);
        assert_eq!(acts(&p), [(Action::Upload, "a")]);
        // 大小相同、远端更新 → 不动（远端有人改过，不该被覆盖）
        let p = build_plan(&[f("a", 5, 100)], &[f("a", 5, 200)], &o, false);
        assert!(p.is_empty());
        // 大小相同、本地更新（超过容差）→ 传
        let p = build_plan(&[f("a", 5, 300)], &[f("a", 5, 100)], &o, false);
        assert_eq!(acts(&p), [(Action::Upload, "a")]);
    }

    #[test]
    fn 计划_mtime_容差内不动() {
        let o = Opts { delete: false, mtime_slack: 2 };
        // 本地只新 1 秒：容差内，判为无变化
        let p = build_plan(&[f("a", 5, 201)], &[f("a", 5, 200)], &o, false);
        assert!(p.is_empty(), "{:?}", acts(&p));
    }

    #[test]
    fn 计划_类型冲突不猜() {
        let p = build_plan(&[d("a", 1)], &[f("a", 5, 5)], &Opts::default(), false);
        assert_eq!(p.n_conflict, 1);
        assert!(acts(&p).is_empty(), "冲突项不该出现在执行序列里");
        // 反过来也一样
        let p = build_plan(&[f("a", 5, 5)], &[d("a", 1)], &Opts::default(), false);
        assert_eq!(p.n_conflict, 1);
    }

    #[test]
    fn 计划_远端多出来默认不动() {
        let p = build_plan(&[], &[f("extra.txt", 1, 1)], &Opts::default(), false);
        assert_eq!(p.remote_only, ["extra.txt"]);
        assert!(p.is_empty());
        assert!(p.summary(false).contains("未勾选删除"));
    }

    #[test]
    fn 计划_勾了删除才列待删() {
        let o = Opts { delete: true, ..Opts::default() };
        let p = build_plan(&[], &[f("extra.txt", 1, 1), d("olddir", 1)], &o, false);
        assert_eq!(p.n_delete, 2);
        assert!(p.remote_only.is_empty());
    }

    #[test]
    fn 计划_远端根不存在时把根也建出来() {
        let p = build_plan(&[f("a", 1, 1)], &[], &Opts::default(), true);
        let seq = acts(&p);
        assert_eq!(seq[0], (Action::Mkdir, ""));
        assert!(p.summary(true).contains("远程目录不存在"));
    }

    #[test]
    fn 顺序_先建目录再上传最后删() {
        let o = Opts { delete: true, ..Opts::default() };
        let local = vec![d("a/b", 1), f("a/b/x", 1, 1), d("a", 1)];
        let remote = vec![d("old", 1), d("old/deep", 1), f("old/deep/y", 1, 1)];
        let p = build_plan(&local, &remote, &o, true);
        assert_eq!(
            acts(&p),
            [
                (Action::Mkdir, ""),
                (Action::Mkdir, "a"),
                (Action::Mkdir, "a/b"),
                (Action::Upload, "a/b/x"),
                (Action::Delete, "old/deep/y"),
                (Action::Delete, "old/deep"),
                (Action::Delete, "old"),
            ]
        );
    }

    #[test]
    fn 顺序_删除按深度从深到浅() {
        let o = Opts { delete: true, ..Opts::default() };
        let p = build_plan(&[], &[d("a", 1), d("a/b", 1), d("a/b/c", 1)], &o, false);
        let dels: Vec<&str> = order(&p)
            .into_iter()
            .filter(|i| i.action == Action::Delete)
            .map(|i| i.rel.as_str())
            .collect();
        assert_eq!(dels, ["a/b/c", "a/b", "a"]);
    }

    #[test]
    fn 路径_拼接不吃掉斜杠() {
        assert_eq!(remote_path("/srv/app", "a/b.txt"), "/srv/app/a/b.txt");
        assert_eq!(remote_path("/srv/app/", "a/b.txt"), "/srv/app/a/b.txt");
        assert_eq!(remote_path("", "a"), "/a");
        assert_eq!(remote_path("/", "a"), "/a");
        assert_eq!(remote_path("/srv/app", ""), "/srv/app");
    }

    #[test]
    fn 预览_默认不列无变化() {
        let p = build_plan(&[f("same", 1, 1), f("new", 1, 1)], &[f("same", 1, 1)], &Opts::default(), false);
        assert_eq!(p.visible(false).len(), 1);
        assert_eq!(p.visible(true).len(), 2);
        assert_eq!(p.n_same, 1);
    }

    #[test]
    fn 摘要_上传数与字节都对() {
        let p = build_plan(&[f("a", 100, 1), f("b", 200, 1)], &[], &Opts::default(), false);
        assert!(p.summary(false).contains("上传 2 个"), "{}", p.summary(false));
        assert!(p.summary(false).contains("300"), "{}", p.summary(false));
    }
}
