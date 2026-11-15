//! **拖动移动**（阶段 106）的冒烟：把文件/文件夹**拖进另一个文件夹**。
//!
//! **为什么要有**：文件管理器里最常用的动作之一——拖到某个文件夹上就
//! 移进去。以前这个程序里**同面板自拖被直接忽略**（拖来拖去什么都不会
//! 发生），用户只能一个一个用「重命名」改名字。
//!
//! **验的是什么**（真控件、真路径）：
//! 1. **命中测试**：落点在**文件夹那一行**上 ⇒ 那个目录的绝对路径；
//! 2. 落点在**文件那一行**上 ⇒ `None`（文件不是"移进去"的目标）；
//! 3. 落点在 `..`（上级目录）那一行上 ⇒ `None`（它是导航，不是目录）；
//! 4. **本地移动真的发生**：把文件拖进子目录 ⇒ 文件真的挪过去了；
//! 5. **同名不覆盖**（用户 2026-11-14 拍板）：目标目录里已有同名 ⇒
//!    报错停下，**一个字节都不动**。
//!
//! ⚠️ **GTK 没有"模拟一次拖放"的 API** ⇒ 拖放动作本身没法自动化；
//! 这里把「命中测试」（落点 → 目标目录）与「移动」（路径 → 结果）分开验，
//! 两段合起来就是拖动移动的完整链路。
//!
//! ```text
//! xvfb-run -a cargo run --example drag_move_smoke
//! ```

use gtk::prelude::*;
use puttyplus::args::ConnOptions;
use puttyplus::panel::pane;

/// 退出时把临时目录删掉（别留垃圾）。
struct TempDir(std::path::PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// 按名字找到模型里的那一行，返回行下标。
fn row_of(pane: &pane::Pane, name: &str) -> Option<usize> {
    let mut it = pane.model.iter_first();
    let mut idx = 0usize;
    while let Some(mut i) = it {
        let n: String = pane.model.get(&i, pane::COL_NAME);
        if n == name {
            return Some(idx);
        }
        idx += 1;
        it = if pane.model.iter_next(&mut i) {
            Some(i)
        } else {
            None
        };
    }
    None
}

/// 那一行的**纵坐标**（相对列表的坐标；拖放落点就是这个坐标系）。
///
/// ⚠️ 必须用**真实控件**量（`cell_area`）⇒ 手算的行高会跟实际布局差几像素
/// ⇒ 命中测试就落不到那一行上（冒烟会假红）。
fn row_y(pane: &pane::Pane, idx: usize) -> i32 {
    let path = gtk::TreePath::from_indices(&[idx as i32]);
    let rect = pane.view.cell_area(Some(&path), Some(&pane.name_col));
    rect.y() + rect.height() / 2
}

fn main() {
    // ⚠️ **别碰用户真实的设置文件**。
    let settings_ini = std::env::temp_dir()
        .join(format!("pp-drag-move-smoke-set-{}.ini", std::process::id()));
    let _ = std::fs::remove_file(&settings_ini);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &settings_ini) };

    // 造一个临时目录树：`dir1/`（子目录）+ `file.txt`
    let root = std::env::temp_dir().join(format!("pp_drag_move_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir1 = root.join("dir1");
    std::fs::create_dir_all(&dir1).expect("建临时目录");
    let file = root.join("file.txt");
    std::fs::write(&file, b"hello\n").expect("写临时文件");
    let _guard = TempDir(root.clone());

    gtk::init().expect("gtk init");
    puttyplus::theme::apply(puttyplus::theme::Theme::Light);

    let tabs = puttyplus::tabs::Tabs::new();
    let opts = ConnOptions {
        proto: "ssh".into(),
        host: "127.0.0.1".into(),
        port: 9,
        user: "nobody".into(),
        ..Default::default()
    };
    tabs.open(&opts);
    let win = gtk::Window::new();
    win.set_title(Some("drag move smoke"));
    win.set_default_size(1000, 700);
    win.set_child(Some(&tabs.notebook));
    win.present();
    pump(600);

    let st = tabs.current_panel_state().expect("应有当前面板");

    // ⚠️ **让本地面板指着那个临时目录**（它默认是用户 home ⇒ 不能拿它测）。
    let cwd = root.to_string_lossy().to_string();
    *st.local_cwd.borrow_mut() = cwd.clone();
    puttyplus::panel::fill_local_pane(&st);
    pump(300);

    let p = st.pane(false);

    // ---- 1) 落在「文件夹」那一行上 ⇒ 那个目录的绝对路径 ----
    let dir_idx = row_of(&p, "dir1").expect("列表里应当有 dir1");
    let y = row_y(&p, dir_idx);
    let hit = pane::drop_target_row(&p, &cwd, 20, y);
    assert_eq!(
        hit.as_ref().map(|(_, d)| d.as_str()),
        Some(dir1.to_string_lossy().as_ref()),
        "落在文件夹行上却没命中那个目录"
    );
    println!("[1] 命中测试：文件夹行 ⇒ {d} ✓", d = dir1.display());

    // ---- 2) 落在「文件」那一行上 ⇒ None ----
    let file_idx = row_of(&p, "file.txt").expect("列表里应当有 file.txt");
    let fy = row_y(&p, file_idx);
    assert!(
        pane::drop_target_row(&p, &cwd, 20, fy).is_none(),
        "文件行不该是移动目标（拖到文件上没意义）"
    );
    println!("[2] 命中测试：文件行 ⇒ 不作目标 ✓");

    // ---- 3) 落在 ".." 那一行上 ⇒ None（它是导航，不是目录）----
    let up_y = row_y(&p, 0);
    assert!(
        pane::drop_target_row(&p, &cwd, 20, up_y).is_none(),
        "\"..\" 不该是移动目标（它是「进上级目录」的导航行）"
    );
    println!("[3] 命中测试：\"..\" 行 ⇒ 不作目标 ✓");

    // ---- 4) 本地移动真的发生 ----
    let moved = puttyplus::panel::move_local_into(
        &st,
        &[file.to_string_lossy().to_string()],
        &dir1.to_string_lossy(),
    );
    let _ = moved;
    pump(300);
    assert!(
        !file.exists(),
        "源文件还在（没被移走）：{}",
        file.display()
    );
    assert!(
        dir1.join("file.txt").exists(),
        "文件没进目标目录：{}",
        dir1.join("file.txt").display()
    );
    println!("[4] 本地移动：文件真的进了 dir1 ✓");

    // ---- 5) 同名不覆盖（用户拍板）：报错停下，一个字节都不动 ----
    //
    // ⚠️ 步骤 4 已经把 `file.txt` 移进 `dir1` 了 ⇒ 现在再往 `dir1` 移一个
    // **同名**的 `file.txt` 就是冲突 ⇒ 目标文件不能被覆盖、源文件不能动。
    //
    // ⚠️ 冲突文件的**名字必须跟源文件一样**（写别的名字就测不到冲突了
    // ——反向验证时才发现这个坑）。
    assert_eq!(
        std::fs::read(dir1.join("file.txt")).unwrap(),
        b"hello\n",
        "前提不对：目标文件的内容不是预期的"
    );
    std::fs::write(&file, b"new\n").expect("写源文件");
    puttyplus::panel::move_local_into(
        &st,
        &[file.to_string_lossy().to_string()],
        &dir1.to_string_lossy(),
    );
    pump(300);
    assert_eq!(
        std::fs::read(dir1.join("file.txt")).unwrap(),
        b"hello\n",
        "同名文件被覆盖了（用户定的：不覆盖）"
    );
    assert!(file.exists(), "源文件被动了（同名时不该动它）");
    println!("[5] 同名不覆盖：报错停下、旧文件没被动 ✓");

    println!("drag_move_smoke: OK");
}
