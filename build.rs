//! 构建脚本（阶段 120）：给 Windows 可执行文件嵌图标 + 版本资源。
//!
//! ⚠️ **目标平台判定要看 `CARGO_CFG_TARGET_OS`，不能用 `#[cfg]`**：
//!    build.rs 编译运行在**宿主**上，`#[cfg(windows)]` 判的是宿主 ⇒
//!    在 Linux 上交叉编译 Windows 目标时会漏、在 Windows 上编 Linux
//!    目标时会错。Cargo 只对 build.rs 暴露 `CARGO_CFG_*` 环境变量。
//!
//! Linux 上什么都不做 ⇒ CI 的 `cargo check --examples`、deb/tar.gz 打包
//! 完全不受影响。Windows 下 131 个 example 的 exe 也会带上同一份资源，
//! 无害（每个才 4KB）。
//!
//! 图标真源与生成办法见 `tools/gen_windows_icon.py`（从 src/appicon.rs
//! 的内嵌 PNG 生成，`assets/puttyplus.ico` 是生成物、随库提交）。

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    println!("cargo:rerun-if-changed=assets/puttyplus.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/puttyplus.ico");
    // 版本号取 Cargo.toml（release.yml 有 tag ⟷ Cargo.toml ⟷ debian/changelog
    // 的一致性闸门 ⇒ 这里跟着 Cargo.toml 走就是对的）。
    // ⚠️ 资源串保持 ASCII：rc.exe 对非 ASCII 的编码规则容易被坑，
    //    版本资源里没必要放中文（用户可见的中文在 GUI 里）。
    res.set("FileDescription", "PuTTYPlus");
    res.set("ProductName", "PuTTYPlus");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_default();
    res.set("FileVersion", &version);
    res.set("ProductVersion", &version);
    if let Err(e) = res.compile() {
        panic!(
            "嵌图标/版本资源失败：{e}\n\
             （MSVC 工具链需要 Windows SDK 的 rc.exe；CI 的 msvc-dev-cmd 自带，\n\
             本机没有的话把 Windows Kits\\10\\bin\\<版本>\\x64 加进 PATH）"
        );
    }
}
