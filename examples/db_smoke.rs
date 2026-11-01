//! 数据库通道直连冒烟（阶段 7.2 的 PoC）。
//!
//! 证明四件事：
//!   1. 在既有 SSH 连接上开 `direct-tcpip` 通道，**不占固定端口**（PG 走
//!      一次性 Unix 套接字；MySQL 走随机回环端口——MySQL 的认证方式决定
//!      了必须是 TCP，详见 `backend::db::open_mysql`）；
//!   2. 通道就是普通双向流，能承载 PostgreSQL 协议（startup / auth / query）；
//!   3. 通道打开期间同一条连接上的 SFTP 照常工作（真正的多路复用）；
//!   4. 带 `--sql` 时走生产路径 `backend::db`：通道 → 本地一次性桥接
//!      （PG：Unix 套接字；MySQL：回环 TCP）→ 驱动，拿到真实结果集。
//!
//! 运行：
//!   cargo run --example db_smoke -- <ssh_host> <ssh_port> <user> \
//!       [--key FILE] [--db HOST:PORT] [--proto pg|mysql|raw] \
//!       [--sql "select 1"] [--db-user U] [--dbname D] [--db-pass P] \
//!       [--limit N] [--fake-pg] [--expect-need-password]
//!
//! `--expect-need-password`：**故意不带密码**连库，验证服务端要密码时
//! 驱动错误被翻译成「要密码」（UI 据此弹密码框），而不是一句看不懂的
//! "invalid configuration"。
//!
//! 默认目标是 127.0.0.1:22（跳板机自己的 sshd），raw 模式读 SSH 横幅。
//! `--proto pg` 发 PostgreSQL StartupMessage，期待 `R`（认证请求）或 `E`
//! （错误响应）——两者都证明数据库收到了完整的应用层请求。
//! `--proto mysql` 等服务器主动发来的 greeting（首字节 10）。
//! `--fake-pg` 在本机起一个假 Postgres（认证 + 450 行结果，支持游标分页），
//! 没有真库时用 `--sql` 跑通完整查询链路；仅当 sshd 与测试程序同机时有效。
//! MySQL 走**回环 TCP**桥，需要真库：`--proto mysql --db host:3306
//! --sql "select 1" --db-user root --db-pass ...`
//!
//! 说明：`--fake-pg` 的监听端口是**假数据库**，不是客户端隧道端口。
//! PG 的客户端侧不占 TCP 端口（一次性 Unix 套接字）；MySQL 的客户端侧
//! 会占一个**随机回环端口**（只绑 127.0.0.1，会话结束即释放）——
//! `caching_sha2_password` 在服务端把 TCP 连接视为"不安全"时只认 RSA
//! 公钥交换，驱动必须以为自己在走 TCP 才会走那条路（原因见
//! `backend::db::open_mysql` 的注释）。

use std::path::Path;
use std::time::Duration;

use puttyplus::args::{AuthMethod, AuthOrder};
use puttyplus::backend::conn;
use puttyplus::backend::db::{DbConfig, DbProto, DbRows, DbSession};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone, Copy, PartialEq)]
enum Proto {
    Pg,
    MySql,
    Raw,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut pos: Vec<String> = Vec::new();
    let mut key: Option<String> = None;
    let mut db: Option<String> = None;
    let mut proto: Option<Proto> = None;
    let mut sql: Option<String> = None;
    let mut db_user: Option<String> = None;
    let mut db_name: Option<String> = None;
    let mut db_pass: Option<String> = None;
    let mut limit: usize = 100;
    let mut fake_pg = false;
    // 这次**故意不带密码**连库，验证驱动错误被翻成「要密码」。
    let mut expect_need_password = false;

    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--key" => key = args.next(),
            "--db" => db = args.next(),
            "--proto" => {
                proto = Some(match args.next().as_deref() {
                    Some("pg") | Some("postgres") => Proto::Pg,
                    Some("mysql") => Proto::MySql,
                    Some("raw") => Proto::Raw,
                    other => {
                        return Err(format!("--proto 只认 pg/mysql/raw，实得 {other:?}").into());
                    }
                })
            }
            "--sql" => sql = args.next(),
            "--db-user" => db_user = args.next(),
            "--dbname" => db_name = args.next(),
            "--db-pass" => db_pass = args.next(),
            "--limit" => {
                limit = args.next().ok_or("--limit 需要数字")?.parse()?;
            }
            "--fake-pg" => fake_pg = true,
            "--expect-need-password" => expect_need_password = true,
            s if s.starts_with('-') => return Err(format!("未知参数: {s}").into()),
            s => pos.push(s.to_string()),
        }
    }

    let host = pos.first().cloned().unwrap_or_else(|| "127.0.0.1".into());
    let port: u16 = pos.get(1).map(|s| s.parse()).transpose()?.unwrap_or(2222);
    let user = pos
        .get(2)
        .cloned()
        .unwrap_or_else(|| std::env::var("USER").unwrap_or_else(|_| "user".into()));

    // 本地用户态 sshd 的默认密钥；不存在就交给 agent/默认密钥
    let key = key.or_else(|| {
        let p = format!("/tmp/pp_selftest/{user}_ed25519");
        Path::new(&p).exists().then_some(p)
    });

    let (db_host, db_port, fake_task) = if fake_pg {
        let (port, h) = start_fake_pg().await?;
        println!("    假 Postgres 监听 127.0.0.1:{port}（测试替身，不是隧道口）");
        ("127.0.0.1".to_string(), port, Some(h))
    } else if let Some(spec) = db {
        let (h, p) = spec.rsplit_once(':').ok_or("--db 需要 HOST:PORT")?;
        (h.to_string(), p.parse()?, None)
    } else {
        ("127.0.0.1".to_string(), 22, None)
    };

    // 没显式给 proto 时按端口猜：5432/PG、3306/MySQL，其余按 raw；
    // --fake-pg 固定走 PG（假库只认 StartupMessage）
    let proto = if fake_pg {
        Proto::Pg
    } else {
        proto.unwrap_or(match db_port {
            5432 => Proto::Pg,
            3306 => Proto::MySql,
            _ => Proto::Raw,
        })
    };

    let backend = conn::connect(
        &host,
        i32::from(port),
        &user,
        None,
        key.as_deref(),
        None,
        30,
        AuthMethod::default(),
        AuthOrder::default(),
    )
    .await?;
    println!("[1] SSH 连接就绪：{}", backend.id);

    let mut stream = backend.open_tcp_stream(&db_host, db_port).await?;
    println!("[2] direct-tcpip 通道已打开 → {db_host}:{db_port}（不占固定端口）");

    // 通道开着的同时跑 SFTP：两条通道共享同一条 TCP 连接
    let entries = backend.list("/").await?;
    assert!(!entries.is_empty(), "SFTP 列表为空，无法证明多路复用");
    println!("[3] 同一条连接上 SFTP 并发可用：/ 下 {} 项", entries.len());

    if expect_need_password {
        // ---- 「故意不带密码」：验证驱动错误被翻译成了「要密码」 ----
        //
        // **为什么要有这个开关**（2026-10 实测）：PG 驱动在"服务端要密码、
        // 我们没给"时抛的是 `Kind::Config`，原文 "invalid configuration"——
        // 用户完全看不懂。UI 靠 `DbOpenError::needs_password()` 弹密码框，
        // 这个判定必须对着**真库**验过，不能只在单测里自说自话。
        let cfg = DbConfig {
            proto: match proto {
                Proto::Pg | Proto::Raw => DbProto::Postgres,
                Proto::MySql => DbProto::MySql,
            },
            host: db_host.clone(),
            port: db_port,
            user: db_user.clone().unwrap_or_else(|| "postgres".into()),
            dbname: db_name.clone().unwrap_or_else(|| "postgres".into()),
            password: None, // **故意不给**
        };
        match DbSession::open(stream, &cfg).await {
            Ok(_) => {
                return Err(
                    "本该被拒（服务端要密码），却连上了——服务器可能是免密认证，\
                     这条验证对它没意义"
                        .into(),
                );
            }
            Err(e) => {
                println!("[4] 不带密码连库被拒：{}", e.message());
                if !e.needs_password() {
                    return Err(format!(
                        "本该识别成「要密码」（UI 会弹密码框），实得：{}",
                        e.message()
                    )
                    .into());
                }
                println!("    ✓ 识别成「要密码」——UI 会弹密码框让用户当场输");
            }
        }
    } else if let Some(sql) = sql {
        let cfg = DbConfig {
            proto: match proto {
                Proto::Pg | Proto::Raw => DbProto::Postgres,
                Proto::MySql => DbProto::MySql,
            },
            host: db_host.clone(),
            port: db_port,
            user: db_user.unwrap_or_else(|| "postgres".into()),
            dbname: db_name.unwrap_or_else(|| "postgres".into()),
            password: db_pass.map(zeroize::Zeroizing::new),
        };
        let mut sess = DbSession::open(stream, &cfg).await?;
        println!("[4] SQL：{sql}");
        let mut page = sess.query(&sql, limit).await?;
        print_rows(&page);
        let mut pages = 1;
        while !page.done && pages < 100 {
            page = sess.next_page(limit).await?;
            print_rows(&page);
            pages += 1;
        }
        sess.close().await;
    } else {
        match proto {
            Proto::Raw => {
                let b = read_some(&mut stream, 64).await?;
                assert!(!b.is_empty(), "目标没有发任何字节");
                let text = String::from_utf8_lossy(&b);
                println!("[4] raw 读回 {} 字节：{}", b.len(), text.trim());
                if text.starts_with("SSH-2.0-") {
                    println!("    （SSH 横幅经通道到达，目标 = 服务器自己的 sshd）");
                }
            }
            Proto::MySql => {
                let b = read_some(&mut stream, 5).await?;
                assert!(
                    b.len() >= 5 && b[0] == 10,
                    "MySQL greeting 首字节应为协议版本 10，实得 {b:?}"
                );
                println!("[4] MySQL greeting 到达（协议版本 {}）", b[0]);
            }
            Proto::Pg => {
                let pkt = pg_startup("smoke", "postgres");
                stream.write_all(&pkt).await?;
                stream.flush().await?;
                let mut head = [0u8; 5];
                tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut head))
                    .await??;
                match head[0] {
                    b'R' => println!("[4] PostgreSQL 认证请求到达（R），库可达"),
                    b'E' => println!("[4] PostgreSQL 错误响应到达（E），库可达（认证/参数被拒）"),
                    other => {
                        return Err(format!("期待的 PG 响应是 R/E，实得 {other:?}").into());
                    }
                }
            }
        }
        drop(stream);
        println!("[5] 通道关闭（drop 即关，无需额外 teardown）");
    }

    if let Some(h) = fake_task {
        h.abort();
    }

    backend.close().await;
    println!("[6] SSH 连接关闭");
    println!("PASS: 数据库通道可用（单条 SSH 连接复用、不占固定端口）");
    Ok(())
}

fn print_rows(rows: &DbRows) {
    let truncated = if rows.truncated { "，已截断" } else { "" };
    let more = if rows.done {
        "已取完"
    } else {
        "还有更多"
    };
    println!(
        "[5] 第 {} 页：{} 行 × {} 列（{} ms，{more}{truncated}）",
        rows.page,
        rows.rows.len(),
        rows.columns.len(),
        rows.elapsed_ms
    );
    if rows.columns.is_empty() {
        if !rows.command.is_empty() {
            println!("    {}", rows.command);
        }
        return;
    }
    println!("    {}", rows.columns.join(" | "));
    for r in &rows.rows {
        let cells: Vec<&str> = r.iter().map(|v| v.as_deref().unwrap_or("NULL")).collect();
        println!("    {}", cells.join(" | "));
    }
}

async fn read_some<S>(s: &mut S, n: usize) -> Result<Vec<u8>, Box<dyn std::error::Error>>
where
    S: AsyncRead + Unpin,
{
    let mut buf = vec![0u8; n];
    let k = tokio::time::timeout(Duration::from_secs(5), s.read(&mut buf)).await??;
    buf.truncate(k);
    Ok(buf)
}

/// PostgreSQL 3.0 StartupMessage（user/database 两个参数）。
fn pg_startup(user: &str, db: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&196608u32.to_be_bytes()); // protocol 3.0
    for (k, v) in [("user", user), ("database", db)] {
        body.extend_from_slice(k.as_bytes());
        body.push(0);
        body.extend_from_slice(v.as_bytes());
        body.push(0);
    }
    body.push(0);

    let mut out = Vec::with_capacity(body.len() + 4);
    out.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// 极简假 Postgres：AuthenticationOk → ReadyForQuery，之后：
/// - BEGIN/DECLARE/COMMIT/ROLLBACK/CLOSE → CommandComplete + Ready
/// - FETCH FORWARD n → 从 5 行假数据里取下一页（用于验证游标分页）
/// - 其它 Query → 固定两行结果（一次性路径）
/// 假数据总量：450 行，够跑三页（每页 200）。
const FAKE_TOTAL: usize = 450;

fn fake_rows(start: usize, count: usize) -> Vec<(String, String)> {
    (start..start + count)
        .map(|i| (i.to_string(), format!("row-{i}")))
        .collect()
}

async fn start_fake_pg() -> std::io::Result<(u16, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let h = tokio::spawn(async move {
        let Ok((mut s, _)) = listener.accept().await else {
            return;
        };
        // StartupMessage（无类型字节）
        let mut len = [0u8; 4];
        if s.read_exact(&mut len).await.is_err() {
            return;
        }
        let n = u32::from_be_bytes(len) as usize;
        if !(4..=4096).contains(&n) {
            return;
        }
        let mut body = vec![0u8; n - 4];
        if s.read_exact(&mut body).await.is_err() {
            return;
        }
        if !body.starts_with(&196608u32.to_be_bytes()) {
            let _ = s.write_all(&[b'E', 0, 0, 0, 4]).await;
            return;
        }

        let mut hello = Vec::new();
        push_msg(&mut hello, b'R', &0i32.to_be_bytes()); // AuthenticationOk
        push_msg(&mut hello, b'Z', b"I"); // ReadyForQuery
        if s.write_all(&hello).await.is_err() {
            return;
        }

        let mut fetched = 0usize;
        loop {
            let mut typ = [0u8; 1];
            if s.read_exact(&mut typ).await.is_err() {
                return;
            }
            let mut l = [0u8; 4];
            if s.read_exact(&mut l).await.is_err() {
                return;
            }
            let n = u32::from_be_bytes(l) as usize;
            if !(4..=1 << 20).contains(&n) {
                return;
            }
            let mut body = vec![0u8; n - 4];
            if s.read_exact(&mut body).await.is_err() {
                return;
            }
            match typ[0] {
                b'Q' => {
                    let sql = String::from_utf8_lossy(&body).to_string();
                    if !handle_fake_query(&mut s, &sql, &mut fetched).await {
                        return;
                    }
                }
                b'X' => return, // Terminate
                _ => {}
            }
        }
    });
    Ok((port, h))
}

async fn handle_fake_query(s: &mut tokio::net::TcpStream, sql: &str, fetched: &mut usize) -> bool {
    let up = sql
        .trim()
        .trim_end_matches('\0')
        .trim()
        .to_ascii_uppercase();
    if up.starts_with("BEGIN")
        || up.starts_with("DECLARE")
        || up.starts_with("COMMIT")
        || up.starts_with("ROLLBACK")
        || up.starts_with("CLOSE")
    {
        let mut out = Vec::new();
        push_msg(&mut out, b'C', b"\0");
        push_msg(&mut out, b'Z', b"I");
        return s.write_all(&out).await.is_ok();
    }
    if up.starts_with("FETCH") {
        let n = sql
            .split_whitespace()
            .nth(2)
            .and_then(|t| t.parse::<usize>().ok())
            .unwrap_or(2);
        let start = (*fetched).min(FAKE_TOTAL);
        let end = (start + n).min(FAKE_TOTAL);
        *fetched = end;
        let rows = fake_rows(start, end - start);
        let out = fake_result(&rows);
        return s.write_all(&out).await.is_ok();
    }
    let rows = fake_rows(0, 2);
    let out = fake_result(&rows);
    s.write_all(&out).await.is_ok()
}

fn fake_result(rows: &[(String, String)]) -> Vec<u8> {
    let mut out = Vec::new();
    // RowDescription：两列文本
    let mut t = Vec::new();
    t.extend_from_slice(&2i16.to_be_bytes());
    for name in ["n", "label"] {
        t.extend_from_slice(name.as_bytes());
        t.push(0);
        t.extend_from_slice(&0i32.to_be_bytes()); // table oid
        t.extend_from_slice(&0i16.to_be_bytes()); // attr num
        t.extend_from_slice(&25i32.to_be_bytes()); // type oid: text
        t.extend_from_slice(&(-1i16).to_be_bytes()); // typlen
        t.extend_from_slice(&(-1i32).to_be_bytes()); // typmod
        t.extend_from_slice(&0i16.to_be_bytes()); // format: text
    }
    push_msg(&mut out, b'T', &t);

    for (n, label) in rows {
        let mut d = Vec::new();
        d.extend_from_slice(&2i16.to_be_bytes());
        for v in [n, label] {
            d.extend_from_slice(&(v.len() as i32).to_be_bytes());
            d.extend_from_slice(v.as_bytes());
        }
        push_msg(&mut out, b'D', &d);
    }
    push_msg(
        &mut out,
        b'C',
        format!("SELECT {}\0", rows.len()).as_bytes(),
    );
    push_msg(&mut out, b'Z', b"I");
    out
}

fn push_msg(out: &mut Vec<u8>, typ: u8, body: &[u8]) {
    out.push(typ);
    out.extend_from_slice(&(body.len() as u32 + 4).to_be_bytes());
    out.extend_from_slice(body);
}
