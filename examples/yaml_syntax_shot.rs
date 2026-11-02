//! YAML 语法高亮截图（用户 2026-11-02「增加 yml 的语法高亮」）。
//!
//! 打开一个内容"什么都有"的 `.yml`，截图供人眼核对：
//! 文档分隔符、键、序列项、块标量（含内容）、标签、锚点、日期时间、注释。
//!
//! ```text
//! xvfb-run -a cargo run --example yaml_syntax_shot
//! ```

#![allow(deprecated)]

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::{self, fileedit, SftpPanel};
use puttyplus::theme::{self, Theme};

const SAMPLE: &str = r#"---
# 一份 docker-compose 风格的文件
name: 演示服务
version: "3.9"

services:
  web:
    image: nginx:1.25-alpine
    ports:
      - "8080:80"
      - "8443:443"
    restart: unless-stopped
    environment:
      TZ: Asia/Shanghai
      DEBUG: false
    deploy:
      replicas: 3
    command: |
      echo 启动中
      nginx -g 'daemon off;'
    labels:
      owner: 运维组
  db:
    image: postgres:16
    volumes:
      - pgdata:/var/lib/postgresql/data
    healthcheck:
      test: ["CMD-SHELL", "pg_isready -U postgres"]
      interval: 10s
      retries: 5

volumes:
  pgdata: {}

# 日期与时间
release: 2024-01-31
built_at: 12:30:00
timeout: 1.5e-3
defaults: &defaults
  restart: always
child:
  <<: *defaults
  restart: never
empty_value: ~
tagged: !!str 123
...
"#;

fn pump(ms: u64) {
    let ctx = gtk::glib::MainContext::default();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < deadline {
        while ctx.pending() {
            ctx.iteration(false);
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn shot(path: &str) {
    let _ = std::process::Command::new("/usr/bin/scrot")
        .arg("-o")
        .arg(path)
        .status();
    println!("截图：{path}");
}

fn main() {
    gtk::init().expect("gtk init");
    theme::apply(Theme::Dark);

    let dir = std::env::temp_dir().join(format!("pp_yaml_shot_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("compose.yml");
    std::fs::write(&file, SAMPLE).expect("sample");
    let path = file.to_string_lossy().to_string();

    let panel = SftpPanel::new(&ConnOptions::default());

    let win = gtk::Window::new();
    win.set_title(Some("yaml syntax shot"));
    win.set_default_size(1100, 900);
    win.set_child(Some(panel.widget()));
    win.present();
    pump(400);

    panel::fileedit::open_local(&panel.st, &path, false);
    pump(900);
    shot("/tmp/pp_yaml_syntax.png");

    // 编辑器窗口只有 640 高 ⇒ 尾部（日期/锚点/标签）看不见，滚过去再截一张
    {
        let edits = panel.st.file_edits.borrow();
        let e = edits.first().expect("编辑器").clone();
        drop(edits);
        panel::fileedit::jump_to_line(&e, 38);
        pump(600);
        shot("/tmp/pp_yaml_syntax_tail.png");
    }

    panel::shutdown(&panel.st);
    let _ = std::fs::remove_dir_all(&dir);
}
