//! 术语表（阶段 81 / 审计 P2-8）。
//!
//! 同一个东西历史上叫过好几个名字：「会话」/「连接」、「远程」/「远端」、
//! 「记录」/「录制」……用户得自己猜这两个词是不是一回事。
//!
//! 现在**只有这一份表**，界面照着用；用错的说法由 `lib.rs` 里的
//! lint 单测扫出来（`远端` 那条已经钉死了）。
//!
//! | 概念 | 统一用 | 不要用 | 为什么 |
//! |---|---|---|---|
//! | 保存下来的一条主机信息（主机/端口/用户/密钥） | 连接 | 会话 | 「会话」= 打开着的那个标签页 |
//! | 打开着的那个标签页（正在跑的） | 会话 | 连接 | |
//! | 另一台机器上的那一侧 | 远程 | 远端 | 与「本地」成对（远程 / 本地） |
//! | 本机这一侧 | 本地 | 近端 | |
//! | 终端输出的逐行文本 | 日志 | 记录 | 「记录」是动词，不是东西名 |
//! | 屏幕录像（asciicast，能回放） | 录制 | 记录 | |
//! | 把数据读进来 | 导入 | 恢复 | |
//! | 把数据写出去（成文件） | 导出 | 保存 | |
//! | 在两台机器之间搬文件 | 传输 | 上传 / 下载 | 上传/下载说的是**方向** |

/// 保存下来的一条主机信息。
pub const CONNECTION: &str = "连接";
/// 打开着的那个标签页（正在跑的）。
pub const SESSION: &str = "会话";
/// 另一台机器上的那一侧。
pub const REMOTE: &str = "远程";
/// 本机这一侧。
pub const LOCAL: &str = "本地";
/// 终端输出的逐行文本。
pub const LOG: &str = "日志";
/// 屏幕录像（asciicast）。
pub const CAPTURE: &str = "录制";
/// 把数据读进来。
pub const IMPORT: &str = "导入";
/// 把数据写出去。
pub const EXPORT: &str = "导出";
/// 在两台机器之间搬文件。
pub const TRANSFER: &str = "传输";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 术语不重样() {
        let all = [
            CONNECTION, SESSION, REMOTE, LOCAL, LOG, CAPTURE, IMPORT, EXPORT, TRANSFER,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "两个概念用了同一个词：{a}");
            }
        }
    }

    #[test]
    fn 远程与本地成对() {
        // 「远程」得跟「本地」成对（不然用户不知道对面是哪一边）
        assert_eq!(REMOTE, "远程");
        assert_eq!(LOCAL, "本地");
    }
}
