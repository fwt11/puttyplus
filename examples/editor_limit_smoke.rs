//! 内置查看/编辑器的**大小上限可配置**冒烟（用户 2026-11-11 提的）。
//!
//! 需求原文：「编辑器查看上限改为可配置」。
//!
//! 以前上限是**硬编码 8 MiB**（代码注释写着「不给用户改——调大只会让
//! `TextView` 卡死」）⇒ 现在给用户一个开关（首选项 →「高级」页）。
//!
//! 这个冒烟钉的是**三件事**（光看设置项的值不算数）：
//!
//! 1. 设置项写进 `settings.ini` 之后，`settings::editor_max_bytes()` 读得到；
//! 2. **编辑器用的确实是这个值**（`fileedit::max_bytes()` 不是写死的 8 MiB）；
//! 3. **上限真的挡住了打开文件**（这是关键：改了设置但打开大文件还是成功
//!    ⇒ 上限没接上，只是多了个摆设）。
//!
//! 运行：
//!
//! ```text
//! cargo run --example editor_limit_smoke
//! ```

use std::path::PathBuf;

/// 写一份 `settings.ini`（只写 `[editor] max_bytes` 一项）。
fn write_settings(ini: &PathBuf, max_bytes: u64) {
    let body = format!("[editor]\nmax_bytes={max_bytes}\n");
    std::fs::write(ini, body).expect("写临时设置文件");
}

fn main() {
    // ⚠️ **别碰用户真实的设置文件**（规则 6）：整个冒烟都指向临时文件。
    let ini = std::env::temp_dir().join(format!("pp-editor-limit-{}.ini", std::process::id()));
    let big = std::env::temp_dir().join(format!("pp-editor-limit-{}.bin", std::process::id()));
    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&big);
    unsafe { std::env::set_var("PUTTYPLUS_SETTINGS_INI", &ini) };

    // ---- 1) 默认值：没写设置时是 8 MiB ----
    assert_eq!(
        puttyplus::settings::editor_max_bytes(),
        puttyplus::settings::DEFAULT_EDITOR_MAX_BYTES,
        "没设过时应当是默认值"
    );

    // ---- 2) 写成 64 KiB ⇒ 设置与编辑器都读得到 ----
    write_settings(&ini, 64 * 1024);
    assert_eq!(
        puttyplus::settings::editor_max_bytes(),
        64 * 1024,
        "设置项写进去的值没被读出来"
    );
    assert_eq!(
        puttyplus::panel::fileedit::max_bytes(),
        64 * 1024,
        "编辑器用的上限不是设置里的那个（还是写死的？）"
    );

    // ---- 3) **上限真的挡住了打开文件** ----
    //
    // ⚠️ 这是这条冒烟的关键：改了设置但大文件照样能打开 ⇒ 上限没接上。
    let big_size = 128 * 1024;
    std::fs::write(&big, vec![b'a'; big_size]).expect("写大文件");
    let path = big.to_str().expect("路径");
    let err = puttyplus::panel::fileedit::local_read_err(path)
        .expect("超过上限的文件应当被挡住（却读成功了）");
    assert!(
        err.contains("超过内置查看/编辑上限"),
        "提示语不对：{err}"
    );
    assert!(
        err.contains("64.0 KB"),
        "提示里要说的是**配置出来的上限**（64.0 KB），不是写死的那个：{err}"
    );
    assert!(
        !err.contains("8.0 MB"),
        "提示里还挂着默认的 8 MiB ⇒ 上限没换成设置里的那个：{err}"
    );
    println!("[1] 上限 64 KiB 时，128 KiB 的文件被挡住了：{err}");

    // ---- 4) 把上限调大 ⇒ 同一个文件就能读了 ----
    //
    // ⚠️ 这一步是「回退验证」：把 `fileedit::max_bytes()` 改回写死的 8 MiB
    //    （也就是不做这个功能），第 3 步就会红。
    write_settings(&ini, 16 * 1024 * 1024);
    assert_eq!(
        puttyplus::panel::fileedit::max_bytes(),
        16 * 1024 * 1024,
        "改了上限，编辑器没跟上"
    );
    assert!(
        puttyplus::panel::fileedit::local_read_err(path).is_none(),
        "上限调到 16 MiB 后，这个文件应当能打开"
    );
    println!("[2] 上限调到 16 MiB 后，同一个文件能打开了");

    // ---- 5) 写坏的值 ⇒ 回默认（而不是「不限」） ----
    write_settings(&ini, 0);
    assert_eq!(
        puttyplus::settings::editor_max_bytes(),
        puttyplus::settings::EDITOR_MAX_BYTES_FLOOR,
        "0 应当被夹到下限（不是 0＝不限）"
    );
    write_settings(&ini, 999_999_999_999);
    assert_eq!(
        puttyplus::settings::editor_max_bytes(),
        puttyplus::settings::EDITOR_MAX_BYTES_CEILING,
        "越界的值应当被夹到上限（再大真的会冻住 TextView）"
    );
    println!("[3] 越界的值被夹住了（下限 64 KiB / 上限 64 MiB）");

    let _ = std::fs::remove_file(&ini);
    let _ = std::fs::remove_file(&big);
    println!("editor_limit smoke ok");
}
