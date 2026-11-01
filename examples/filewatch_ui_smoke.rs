//! 文件列表外部变更自动刷新冒烟（本地 GIO 监视 + 远端 SFTP 目录 mtime 轮询）。
//!
//! 运行：
//!   xvfb-run -a cargo run --example filewatch_ui_smoke
//!   xvfb-run -a cargo run --example filewatch_ui_smoke -- <host> <port> <user> <keyfile> <远程可写目录>
//!
//! 不带远端参数时只测本地目录；带参数时同时测远端（需用户态 sshd）。
//! 把 `PUTTYPLUS_WATCH_MS` 调小到 300ms，远端轮询不用等 2.5 秒。

#![allow(deprecated)]

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, SftpPanel, pane};

fn pump(ms: u64) {
    let ctx = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn has(st: &std::rc::Rc<panel::PanelState>, remote: bool, name: &str) -> bool {
    let pane = st.pane(remote);
    let mut it = pane.model.iter_first();
    while let Some(mut i) = it {
        if pane.model.get::<String>(&i, pane::COL_NAME) == name {
            return true;
        }
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    false
}

fn main() {
    let mut pos = std::env::args().skip(1);
    let remote_args = if pos.len() >= 5 {
        Some((
            pos.next().unwrap(),
            pos.next().unwrap().parse::<i32>().unwrap(),
            pos.next().unwrap(),
            pos.next().unwrap(),
            pos.next().unwrap(),
        ))
    } else {
        None
    };

    let ldir = std::env::temp_dir().join(format!("pp_filewatch_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ldir);
    std::fs::create_dir_all(&ldir).unwrap();
    std::fs::write(ldir.join("a.txt"), b"x").unwrap();
    unsafe {
        std::env::set_var("PUTTYPLUS_LOCALDIR", &ldir);
        std::env::set_var("PUTTYPLUS_WATCH_MS", "300");
    }

    gtk::init().unwrap();
    puttyplus::theme::apply(puttyplus::theme::Theme::Dark);
    let opts = match &remote_args {
        Some((host, port, user, key, _)) => ConnOptions {
            proto: "ssh".into(),
            host: host.clone(),
            port: *port,
            user: user.clone(),
            keyfile: Some(key.clone()),
            ..Default::default()
        },
        None => ConnOptions::default(),
    };
    let panel = SftpPanel::new(&opts);
    let win = gtk::Window::new();
    win.set_default_size(800, 500);
    win.set_child(Some(panel.widget()));
    win.present();
    pump(600);

    // 本地：外部新增/删除
    assert!(has(&panel.st, false, "a.txt"));
    std::fs::write(ldir.join("local_new.txt"), b"x").unwrap();
    pump(900);
    assert!(has(&panel.st, false, "local_new.txt"), "本地新增没有自动出现");
    std::fs::remove_file(ldir.join("local_new.txt")).unwrap();
    pump(900);
    assert!(!has(&panel.st, false, "local_new.txt"), "本地删除没有自动消失");
    println!("[1] 本地 GIO 监视：新增/删除已自动反映");

    if let Some((_, _, _, _, remote_dir)) = remote_args {
        let mut waited = 0;
        while !panel.st.conn_ok.get() && waited < 12_000 {
            pump(100);
            waited += 100;
        }
        assert!(panel.st.conn_ok.get(), "SFTP 未连接");
        panel::on_entry_activated(&panel.st, true, &remote_dir);
        waited = 0;
        while *panel.st.remote_cwd.borrow() != remote_dir && waited < 5000 {
            pump(100);
            waited += 100;
        }
        assert_eq!(*panel.st.remote_cwd.borrow(), remote_dir);
        pump(700);

        std::fs::write(std::path::Path::new(&remote_dir).join("remote_new.txt"), b"x").unwrap();
        pump(1500);
        assert!(
            has(&panel.st, true, "remote_new.txt"),
            "远端新增没有自动出现"
        );
        std::fs::remove_file(std::path::Path::new(&remote_dir).join("remote_new.txt")).unwrap();
        pump(1500);
        assert!(
            !has(&panel.st, true, "remote_new.txt"),
            "远端删除没有自动消失"
        );
        println!("[2] 远端 SFTP 目录轮询：新增/删除已自动反映");
    }

    panel::shutdown(&panel.st);
    let _ = std::fs::remove_dir_all(&ldir);
    println!("filewatch ui smoke ok");
}
