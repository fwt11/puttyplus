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
}
