//! **本次运行内的密码共享**（阶段 C8）。
//!
//! ## 为什么要有
//!
//! 一个会话里其实有**好几条彼此独立的 SSH 连接**：
//!
//! | 连接 | 在哪儿建 | 走哪条认证 |
//! |---|---|---|
//! | SFTP 面板 | `panel::worker` → `backend::conn::connect` | `backend::conn::authenticate` |
//! | 终端 | `term::spawn_term_inproc` → `backend::shell::open_shell` | 同上 |
//! | **分屏**再一条 | `tabs::split_index` → 又一个 `TermPane` | 同上 |
//!
//! 三条连接**各自认证** ⇒ 认证失败时**各自弹密码框** ⇒ 用户要输三遍
//! （用户 2026-11-08 在 Windows 上报的就是这个：「打开 sftp 和终端要
//! 分别输入密码，分屏也要输入密码」）。
//!
//! ## 两条机制
//!
//! 1. **密码缓存**（[`get`] / [`put`]）：按 **(host, port, user)** 认人，
//!    谁先连上、输了密码，后面那条连接直接拿来用。
//! 2. **密码框去重**（[`ask_once`]）：同一条会话**只弹一个框**——后到的
//!    连接挂进等待队列，框里一有答案大家一起走。
//!
//! ## 边界（很重要）
//!
//! - **只留在内存里**：进程退出即消失，**不落盘**。落盘是凭据库的事
//!   （`vault`）——「记住密码」是另一回事，别混。
//! - **按 host/port/user 认人**：绝不跨机器串密码。
//! - 密码一律 `Zeroizing`：释放时抹零。

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// 一条会话的身份：**host + port + user**。
///
/// ⚠️ 三样都要：同一个 host 上换个账号是另一条会话（密码不一样）。
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub host: String,
    pub port: i32,
    pub user: String,
}

/// 按 (host, port, user) 造一个会话键。
pub fn key(host: &str, port: i32, user: &str) -> Key {
    Key {
        host: host.to_string(),
        port,
        user: user.to_string(),
    }
}

/* ---------------- 会话级密码文件（给 spawn 出去的 ssh 用） ---------------- */

/// 会话级密码文件的路径。
///
/// ⚠️ **为什么需要文件**（阶段 C8）：
///
/// Linux 主线的终端跑的是**用户自己的 `ssh`**（零回归红线：他的
/// `~/.ssh/config`、agent、ProxyJump 语义全都在）⇒ **认证是 ssh 进程
/// 自己做的**，它在 **tty 上**问密码 ⇒ 我们**看不到**那个密码，也
/// **没法**把它喂回去（往 pty 里写有回显风险：ssh 还没关掉回显时，
/// 密码会明文出现在屏幕上）。
///
/// ⇒ 改让 ssh 问**我们**：`SSH_ASKPASS=<本程序>` +
/// `SSH_ASKPASS_REQUIRE=force`（OpenSSH ≥ 8.4）⇒ ssh 把问题交给
/// **askpass 助手**（＝本程序的另一个进程）⇒ 助手把答案打印给 ssh。
///
/// ⚠️ **助手是另一个进程** ⇒ 拿不到本进程的内存缓存 ⇒ 用一个
/// **会话级临时文件**当通道：
/// - 谁先拿到密码（一般是 SFTP 面板那个对话框）⇒ **写进文件**；
/// - 助手 ⇒ **等文件出现** ⇒ 读出来 ⇒ 打印给 ssh ⇒ 删掉文件。
///
/// ⇒ **这样两个平台的密码体验就一致了**：都是**我们的对话框**问一次，
/// 终端与 SFTP 共用（Linux 上终端的 ssh 也用这一份）。
///
/// ⚠️ 文件放在**用户自己的配置目录**（不是共享的临时目录）⇒ 别的用户
/// 读不到；文件名带 pid ⇒ 多开互不串。
pub fn session_file_path(k: &Key) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    k.hash(&mut h);
    crate::platform::paths::config_dir().join(format!(
        "pp-pw-{}-{:x}.tmp",
        std::process::id(),
        h.finish()
    ))
}

/// 把密码写进会话文件（**父进程**：谁拿到密码谁写）。
pub fn write_session_file(k: &Key, pw: &str) -> bool {
    write_session_file_at(&session_file_path(k), pw)
}

/// 把密码写进**指定路径**的会话文件（测试用：不许写用户真实配置目录）。
pub fn write_session_file_at(path: &Path, pw: &str) -> bool {
    write_private(path, pw.as_bytes())
}

/// 助手进程：等文件出现再读（**等的是父进程写密码**）。
///
/// `timeout` 到了还没出现 ⇒ 返回 `None`（助手只能交白卷 ⇒ ssh 认证失败）。
///
/// ⚠️ **读完把文件清空**（不是删掉）：
///
/// - 清空 ⇒ **密码不留在盘上** ✔；
/// - ⚠️ **ssh 会重试 3 次**（密码错时）⇒ 每次都调一次助手 ⇒ 要是把
///   文件**删掉**，第 2、3 次就会**干等到超时**（终端像卡住了 60 秒）
///   ⇒ 清空之后，第 2、3 次读到的是**空串** ⇒ 立刻失败、不卡 ✔。
pub fn read_session_file_wait(path: &Path, timeout: Duration) -> Option<String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            // ⚠️ **立刻清空**：密码不留盘（且重试时读到的是空串）
            let _ = std::fs::write(path, b"");
            return Some(s);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// 清掉**陈旧**的会话文件（上次崩了/被杀留下的）。
///
/// ⚠️ 只删**一小时前**的 ⇒ 正在用的会话（文件很新）不受影响。
pub fn cleanup_stale_session_files() {
    let dir = crate::platform::paths::config_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.starts_with("pp-pw-") || !name.ends_with(".tmp") {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        // 用修改时间判断年龄（SystemTime 与 Instant 不是一回事 ⇒
        // 直接比 `SystemTime::now().duration_since(modified)`）
        let Ok(age) = std::time::SystemTime::now().duration_since(modified) else {
            continue;
        };
        if age > Duration::from_secs(3600) {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// 写文件，**只有自己能读**（Unix 上是 0600）。
fn write_private(p: &Path, data: &[u8]) -> bool {
    if let Some(dir) = p.parent() {
        if std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = match std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(p)
        {
            Ok(f) => f,
            Err(_) => return false,
        };
        f.write_all(data).is_ok() && f.flush().is_ok()
    }
    #[cfg(not(unix))]
    {
        std::fs::write(p, data).is_ok()
    }
}

/* ---------------- 密码缓存 ---------------- */

thread_local! {
    /// 本次运行内已经问过的密码。
    ///
    /// ⚠️ **thread_local**：只在主线程用（GTK 的对话框与事件都在主线程）
    ///    ⇒ 不用加锁，也不会跨线程共享引用。
    static CACHE: RefCell<HashMap<Key, Zeroizing<String>>> = RefCell::new(HashMap::new());
}

/// 取这条会话已经问过的密码。
pub fn get(k: &Key) -> Option<Zeroizing<String>> {
    CACHE.with(|c| c.borrow().get(k).map(|v| Zeroizing::new(v.to_string())))
}

/// 记住这条会话的密码。
pub fn put(k: &Key, pw: &str) {
    if pw.is_empty() {
        return;
    }
    CACHE.with(|c| {
        c.borrow_mut()
            .insert(k.clone(), Zeroizing::new(pw.to_string()));
    });
}

/// 忘掉这条会话的密码（密码变了、连接被关掉等）。
pub fn forget(k: &Key) {
    CACHE.with(|c| {
        c.borrow_mut().remove(k);
    });
}

/* ---------------- 密码框去重 ---------------- */

thread_local! {
    /// 正在等密码的对话框：会话键 ⇒ 回答后要通知的那些人。
    static ASKING: RefCell<HashMap<Key, Vec<Box<dyn Fn(Option<String>)>>>> =
        RefCell::new(HashMap::new());
}

/// 同一条会话**只弹一个密码框**；后到的连接挂进等待队列。
///
/// - `k`：这条会话的键（host + port + user）。
/// - `on_answer`：拿到密码（`Some`）或用户取消（`None`）时该做的事。
///   **后到的连接靠它**拿到答案——密码框只弹一个，但每条连接都得重连。
/// - `build`：真正弹框。收到一个 `done` 回调：**框里输完密码就调它**
///   （`Some(密码)`），**用户点了取消也调它**（`None`）。
///
/// ⚠️ **取消也要调 `done(None)`**：不然等待者会永远挂着（它们等的是
/// 那个框的答案，不是自己的框）。
pub fn ask_once<F, B>(k: Key, on_answer: F, build: B)
where
    F: Fn(Option<String>) + 'static,
    B: FnOnce(Box<dyn Fn(Option<String>)>) + 'static,
{
    // 已经有同一个框开着 ⇒ 挂进等待队列，别再弹一个（两个框摞在一起
    // 只会让人以为程序出毛病了）。
    let already_asking = ASKING.with(|s| {
        let mut m = s.borrow_mut();
        if m.contains_key(&k) {
            if let Some(list) = m.get_mut(&k) {
                list.push(Box::new(on_answer));
            }
            true
        } else {
            m.insert(k.clone(), vec![Box::new(on_answer)]);
            false
        }
    });
    if already_asking {
        return;
    }
    // 本次是开框的那个。
    let done: Box<dyn Fn(Option<String>)> = Box::new(move |pw: Option<String>| {
        // 先把等待者摘出来（**在通知之前**）——通知里可能会再问一次
        // （比如密码不对要重试），那时应该弹**新**框，而不是复用旧的。
        let list = ASKING.with(|s| s.borrow_mut().remove(&k).unwrap_or_default());
        if let Some(pw) = &pw {
            // 先进缓存 ⇒ 等的人直接能用（不用各自再存一份）。
            put(&k, pw);
        }
        for f in list {
            f(pw.clone());
        }
    });
    build(done);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn 密码按会话存取不串台() {
        let a = key("10.0.0.1", 22, "ops");
        let b = key("10.0.0.2", 22, "ops");
        let c = key("10.0.0.1", 22, "root");
        assert!(get(&a).is_none());
        put(&a, "pw-a");
        assert_eq!(get(&a).map(|s| s.to_string()).as_deref(), Some("pw-a"));
        // 换机器 / 换账号 / 换端口 ⇒ 都不是同一回事
        assert!(get(&b).is_none());
        assert!(get(&c).is_none());
        assert_eq!(get(&key("10.0.0.1", 2222, "ops")), None);
        forget(&a);
        assert!(get(&a).is_none());
    }

    #[test]
    fn 同一会话只弹一个框() {
        let k = key("10.0.0.9", 22, "ops");
        let built = Rc::new(AtomicUsize::new(0));
        let answered = Rc::new(AtomicUsize::new(0));
        let got: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
        // 断言用的副本（原件要 move 进闭包里）
        let built_c = built.clone();
        let answered_c = answered.clone();
        let got_c = got.clone();

        // 第一个：开框
        let b1 = built.clone();
        let a1 = answered.clone();
        let g1 = got.clone();
        let k_outer = k.clone();
        ask_once(
            k.clone(),
            move |pw| {
                a1.fetch_add(1, Ordering::SeqCst);
                if let Some(pw) = pw {
                    g1.borrow_mut().push(pw);
                }
            },
            move |done| {
                b1.fetch_add(1, Ordering::SeqCst);
                // 别的连接这时才来 ⇒ 不该再弹一个框
                let b2 = built.clone();
                let a2 = answered.clone();
                let g2 = got.clone();
                let k2 = k_outer.clone();
                ask_once(
                    k2,
                    move |pw| {
                        a2.fetch_add(1, Ordering::SeqCst);
                        if let Some(pw) = pw {
                            g2.borrow_mut().push(pw);
                        }
                    },
                    move |_done2| {
                        b2.fetch_add(1, Ordering::SeqCst);
                    },
                );
                done(Some("secret".into()));
            },
        );

        assert_eq!(built_c.load(Ordering::SeqCst), 1, "同一条会话只能弹一个框");
        assert_eq!(
            answered_c.load(Ordering::SeqCst),
            2,
            "两个等待者都要被通知"
        );
        assert_eq!(got_c.borrow().len(), 2);
        // 答案同时进缓存 ⇒ 之后的连接直接用
        assert_eq!(
            get(&k).map(|s| s.to_string()).as_deref(),
            Some("secret"),
            "答案必须进缓存，否则下一条连接还得再问一次"
        );
    }

    #[test]
    fn 取消也要通知等待者() {
        let k = key("10.0.0.10", 22, "ops");
        let answered = Rc::new(AtomicUsize::new(0));
        let a = answered.clone();
        ask_once(
            k.clone(),
            move |_pw| {
                a.fetch_add(1, Ordering::SeqCst);
            },
            |done| {
                done(None);
            },
        );
        assert_eq!(answered.load(Ordering::SeqCst), 1, "取消也必须通知");
        assert!(get(&k).is_none(), "取消不该留下密码");
    }

    /// **会话级密码文件**：父进程写、助手等 ⇒ 助手能拿到同一份密码。
    ///
    /// ⚠️ 这是「两个平台只问一次密码」的关键：Linux 上终端的 ssh 是
    /// **另一个进程**（它自己问密码）⇒ 只能靠文件把答案送过去。
    #[test]
    fn 会话文件能通气() {
        use std::time::Duration;
        // ⚠️ **不许写用户真实配置目录** ⇒ 用临时目录
        let dir = std::env::temp_dir().join(format!("pp_pwfile_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("pw.tmp");

        // 助手在等
        let p2 = path.clone();
        let th = std::thread::spawn(move || read_session_file_wait(&p2, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(80));
        // 父进程写
        assert!(write_session_file_at(&path, "s3cret"));

        let got = th.join().unwrap();
        assert_eq!(got.as_deref(), Some("s3cret"), "助手必须拿到父进程写的密码");
        // ⚠️ 读完**清空**（不是删掉）：密码不留在盘上，
        // 而 ssh 重试时读到的是空串（不会干等超时）
        assert_eq!(
            std::fs::read_to_string(&path).unwrap_or_default(),
            "",
            "读完必须清空（密码不留盘 + 重试不卡）"
        );
        // 第二次读 ⇒ 空串（不卡）
        let t0 = std::time::Instant::now();
        assert_eq!(
            read_session_file_wait(&path, Duration::from_millis(200)).as_deref(),
            Some("")
        );
        assert!(t0.elapsed() < Duration::from_secs(2), "重试不该干等");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **助手等不到** ⇒ 返回 None（交白卷），不能干等。
    #[test]
    fn 会话文件等不到就放弃() {
        use std::time::Duration;
        let dir = std::env::temp_dir().join(format!("pp_pwfile2_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("never.tmp");
        let t0 = std::time::Instant::now();
        let got = read_session_file_wait(&path, Duration::from_millis(300));
        assert!(got.is_none(), "等不到就必须放弃");
        assert!(t0.elapsed() < Duration::from_secs(5), "不该干等");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
