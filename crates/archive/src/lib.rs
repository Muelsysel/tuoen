//! `tuoen` 的安全解压与原子安装。
//!
//! 这是 L0 里**最危险的一段代码**：它要把来自网络的归档展开到磁盘上，
//! 任何路径校验的疏漏都是 Zip Slip。所以它的设计不是"小心一点"，
//! 而是**三层各有明确职责的防线**，每一层的必要性都由实测支撑
//! （`research/BSDTAR_SAFETY_MEASURED.md`）。
//!
//! ## 三层
//!
//! | 层 | 在哪 | 挡什么 | 为什么不能省 |
//! |---|---|---|---|
//! | ① 名字校验 | [`name`] | `..` / 绝对路径 / UNC / 盘符 / 保留设备名 / 结尾点空格 / `:` / 控制字符 / 深度 / 长度 | 给出**具体是哪一种违规**（"解压失败"不足以判断是上游问题还是攻击）；而且 bsdtar **明确不挡**保留设备名与结尾点空格 |
//! | ② 解压器 | [`tar`] | `..`、symlink、硬链接、fifo、设备（实测它全拒，并且退出码变 1） | 它比我们更懂格式；但它会**静默改名**（`:` → `_`）与**静默覆盖**（大小写碰撞），所以不能只有它 |
//! | ③ 落盘审计 | [`audit`] | 结果里的 reparse point、普通路径看不见的文件、磁盘上的真实名字、大小写碰撞 | **列表是有损的**（bsdtar 转义控制字符，且转义是双向的），所以"列表里干净"不等于"磁盘上干净" |
//!
//! ## 原子性
//!
//! "要么完整成功，要么完全不留痕迹"拆成两条分别可验证的性质：
//!
//! 1. **最终名字要么完整存在，要么根本不存在** —— 在同一个卷的临时目录里
//!    解压 + 审计，全通过之后才用一次 `rename` 搬过去（见 [`staging`]）；
//! 2. **失败时临时目录被删掉** —— `Drop` 保证，而且带 `\\?\` 兜底
//!    （因为实测恶意归档会造出普通路径删不掉的文件）。
//!
//! ## 格式
//!
//! `.zip` / `.tar.gz` / `.tar.xz` / `.tar.zst` / `.tar.bz2` / **`.7z`**
//! 全部走系统自带 `tar.exe`（bsdtar 3.8.8 / libarchive 3.8.8）。
//!
//! **`.7z` 不是缺口** —— 这一条推翻了早先的设计假设。实测：从阿里镜像
//! 下的 `node-v24.19.0-win-x64.7z`（23,441,591 字节，哈希与上游一致）
//! `tar.exe -tf` 列出 2454 个条目、`-xf` 解出的 `node.exe` 能跑并报
//! `v24.19.0`。所以**不需要内置任何 7z 库**。
//!
//! ## 用法
//!
//! ```no_run
//! use tuoen_archive::{InstallRequest, install_archive, tar::TarCli};
//! use tuoen_platform::{RealFileSystem, SystemProcessRunner};
//!
//! let runner = SystemProcessRunner;
//! let fs = RealFileSystem;
//! let tar = TarCli::probe()?;
//!
//! let request = InstallRequest::new(
//!     r"C:\store\cache\node-24.19.0.zip",
//!     r"C:\store\versions",
//!     "24.19.0",
//! )
//! .stripping(1);
//!
//! let outcome = install_archive(&runner, &fs, &tar, &request)?;
//! println!("装到 {}", outcome.installed_to.display());
//! # Ok::<(), tuoen_archive::ArchiveError>(())
//! ```

pub mod audit;
pub mod error;
pub mod extract;
pub mod limits;
pub mod name;
pub mod staging;
pub mod tar;

pub use audit::{AuditReport, audit_tree};
pub use error::{ArchiveError, NameViolation};
pub use extract::{InstallOutcome, InstallRequest, install_archive};
pub use limits::ExtractLimits;
pub use name::{SafeName, is_reserved_device_name, validate_entry_name};
pub use staging::{StagingDir, long_path, remove_tree};
pub use tar::{EntryKind, ListedEntry, TarCli};

/// 我们支持的归档扩展名。
///
/// 顺序是**检查顺序**，长的在前 —— `.tar.gz` 必须在 `.gz` 之前匹配，
/// 否则 `x.tar.gz` 会被当成 `.gz`。
///
/// 这里只用于**给人看的提示与格式判定**，真正能不能读由 libarchive 说了算
/// （它认的比这张表多）。所以判定失败时不要报"不支持"，要报
/// "扩展名不像我们认得的格式" —— 见 [`ArchiveError::UnknownFormat`]。
pub const KNOWN_EXTENSIONS: &[&str] = &[
    ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".tgz", ".txz", ".tzst", ".tbz2", ".zip", ".7z",
    ".tar",
];

/// 从一个文件名猜格式。
///
/// # Errors
///
/// 扩展名不在 [`KNOWN_EXTENSIONS`] 里。
pub fn guess_format(path: &std::path::Path) -> Result<&'static str, ArchiveError> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    for extension in KNOWN_EXTENSIONS {
        if name.ends_with(extension) {
            return Ok(extension);
        }
    }
    Err(ArchiveError::UnknownFormat {
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn long_extensions_win_over_short_ones() {
        // `.tar.gz` 必须在 `.gz` 之前匹配到，否则 `x.tar.gz` 会被当成 `.gz`。
        // 这张表里没有 `.gz`，但同样的道理适用于 `.tgz` 与 `.tar`。
        assert_eq!(guess_format(Path::new("x.tar.gz")).unwrap(), ".tar.gz");
        assert_eq!(guess_format(Path::new("x.tgz")).unwrap(), ".tgz");
        assert_eq!(guess_format(Path::new("x.zip")).unwrap(), ".zip");
        assert_eq!(guess_format(Path::new("x.7z")).unwrap(), ".7z");
        assert_eq!(guess_format(Path::new("x.tar")).unwrap(), ".tar");
    }

    #[test]
    fn format_guessing_ignores_case() {
        // 上游的资产名大小写不统一（`node-v24.19.0-win-x64.ZIP` 是存在的）。
        assert_eq!(guess_format(Path::new("NODE-V24.ZIP")).unwrap(), ".zip");
        assert_eq!(guess_format(Path::new("X.Tar.Gz")).unwrap(), ".tar.gz");
    }

    #[test]
    fn an_unknown_extension_is_an_archive_at_fault_but_says_the_right_thing() {
        let error = guess_format(Path::new("x.rar")).expect_err("rar 不在表里");
        assert_eq!(error.kind(), "unknown-format");
        // **"不认识"不等于"不支持"**：libarchive 认的比这张表多。
        // 所以消息说的是"扩展名不像我们认得的格式"，而不是"不支持"。
        let text = error.to_string();
        assert!(text.contains("不认识"), "{text}");
        assert!(!text.contains("不支持"), "不该说'不支持'：{text}");
    }

    #[test]
    fn the_extension_table_covers_every_format_the_ticket_names() {
        // 票据 #5 点名的格式：zip / tar.gz / tar.xz / tar.zst / tar.bz2 + 7z。
        for required in [".zip", ".tar.gz", ".tar.xz", ".tar.zst", ".tar.bz2", ".7z"] {
            assert!(
                KNOWN_EXTENSIONS.contains(&required),
                "票据点名的 {required} 不在表里"
            );
        }
    }
}
