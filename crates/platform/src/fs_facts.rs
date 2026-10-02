//! 文件系统事实与 reparse point 判定。
//!
//! **这个模块存在的全部理由**是 `alias-ghost`：本机实测
//! `C:\Users\Muelsyse\AppData\Local\Microsoft\WindowsApps\python.exe`
//! 长度 **0**、reparse tag **`0x8000001b`**，而 `Get-Command python` **成功**并排在 `PATH` 最前
//! （`PATH` 第 8 条，真正的 Python312 在第 36 条）。任何"`Test-Path` 通过就算存在"的实现
//! 都会在这里给出**错误答案** —— 所以存在性、大小、reparse tag 必须一次读全，
//! 而不是分几次"看起来差不多"的检查。
//!
//! **测试实现**：[`crate::fixture::FakeFileSystem`]。真实实现只读。

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::PlatformError;
use crate::sys;

/// `IO_REPARSE_TAG_MOUNT_POINT` —— 目录联接（junction）。**未提权也能创建**（ADR-0001）。
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xa000_0003;
/// `IO_REPARSE_TAG_SYMLINK` —— 符号链接。**未提权且未开 Developer Mode 时创建必失败**（ADR-0001）。
pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xa000_000c;
/// `IO_REPARSE_TAG_APPEXECLINK` —— App Execution Alias。
///
/// 本机 `WindowsApps\python.exe` 就是这个 tag、长度 0。它**既不是文件也不是符号链接**，
/// 所以基于复制的捕获什么也拿不到，而 `Test-Path` 式检查会通过。
pub const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001b;

/// 一个路径是什么形状的 reparse point。
///
/// 序列化形状是**字符串**（`"junction"` / `"app-exec-alias"` / `"other:0x8000001b"`），
/// 这样固定装置（`fixtures/`）可读，且不需要在 TOML 里写一个联合体的形状。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(try_from = "String", into = "String")]
pub enum ReparseKind {
    /// 普通文件或目录，没有 reparse 位。
    #[default]
    None,
    /// 目录联接。本项目的默认版本切换机制。
    Junction,
    /// 指向文件的符号链接。
    SymlinkFile,
    /// 指向目录的符号链接。
    SymlinkDir,
    /// App Execution Alias（长度 0、tag `0x8000001b`）。**不是文件也不是链接**。
    AppExecAlias,
    /// 其它 reparse tag，原样记录。**记录而不猜测**。
    Other(u32),
}

impl ReparseKind {
    /// 从 reparse tag 判定。`is_dir` 用来区分文件链接与目录链接
    /// （两者共用 `IO_REPARSE_TAG_SYMLINK`，靠 `FILE_ATTRIBUTE_DIRECTORY` 分开）。
    #[must_use]
    pub const fn from_tag(tag: u32, is_dir: bool) -> Self {
        match tag {
            IO_REPARSE_TAG_MOUNT_POINT => Self::Junction,
            IO_REPARSE_TAG_SYMLINK => {
                if is_dir {
                    Self::SymlinkDir
                } else {
                    Self::SymlinkFile
                }
            }
            IO_REPARSE_TAG_APPEXECLINK => Self::AppExecAlias,
            other => Self::Other(other),
        }
    }

    /// 取回 tag（`None` 表示没有 reparse 位）。
    #[must_use]
    pub const fn tag(self) -> Option<u32> {
        match self {
            Self::None => None,
            Self::Junction => Some(IO_REPARSE_TAG_MOUNT_POINT),
            Self::SymlinkFile | Self::SymlinkDir => Some(IO_REPARSE_TAG_SYMLINK),
            Self::AppExecAlias => Some(IO_REPARSE_TAG_APPEXECLINK),
            Self::Other(tag) => Some(tag),
        }
    }

    /// 是不是一个"指向别处"的链接（junction / symlink）。
    ///
    /// **App Execution Alias 不算**：它没有目标路径可读，语义也不是重定向。
    #[must_use]
    pub const fn is_link(self) -> bool {
        matches!(self, Self::Junction | Self::SymlinkFile | Self::SymlinkDir)
    }

    /// 是不是 App Execution Alias。**这是 `alias-ghost` 层的唯一判据。**
    #[must_use]
    pub const fn is_app_exec_alias(self) -> bool {
        matches!(self, Self::AppExecAlias)
    }
}

impl fmt::Display for ReparseKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::Junction => f.write_str("junction"),
            Self::SymlinkFile => f.write_str("symlink-file"),
            Self::SymlinkDir => f.write_str("symlink-dir"),
            Self::AppExecAlias => f.write_str("app-exec-alias"),
            Self::Other(tag) => write!(f, "other:0x{tag:08x}"),
        }
    }
}

impl std::str::FromStr for ReparseKind {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "junction" => Ok(Self::Junction),
            "symlink-file" => Ok(Self::SymlinkFile),
            "symlink-dir" => Ok(Self::SymlinkDir),
            "app-exec-alias" => Ok(Self::AppExecAlias),
            other => other
                .strip_prefix("other:")
                .and_then(|text| {
                    text.strip_prefix("0x")
                        .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                        .or_else(|| text.parse::<u32>().ok())
                })
                .map(Self::Other)
                .ok_or_else(|| format!("未知的 reparse 形状：{other}")),
        }
    }
}

impl TryFrom<String> for ReparseKind {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<ReparseKind> for String {
    fn from(value: ReparseKind) -> Self {
        value.to_string()
    }
}

/// 一个路径的完整事实。**一次读全**：存在性、是否目录、大小、reparse tag。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFacts {
    /// 被查的路径（原样带回，方便调用方做归属判断）。
    pub path: PathBuf,
    /// 存在。**注意**：App Execution Alias 在这里是 `true`，而它不是文件 ——
    /// 所以任何判断都必须先看 [`FileFacts::reparse`]。
    pub exists: bool,
    /// 是目录。
    pub is_dir: bool,
    /// 字节数。App Execution Alias 与 junction 都是 0。
    pub size: u64,
    /// reparse 形状。
    pub reparse: ReparseKind,
    /// 链接目标（仅 junction / symlink）。**记录而不猜测**：本机 Oracle 的
    /// `java8path_target_1783390` 就嵌了安装序号，不可预测。
    pub link_target: Option<String>,
    /// 读失败的原因。`exists == false` 且这里非空 = "不是不存在，是读不了"。
    pub error: Option<PlatformError>,
}

impl FileFacts {
    /// 一条"不存在"的事实。
    #[must_use]
    pub fn missing(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            exists: false,
            is_dir: false,
            size: 0,
            reparse: ReparseKind::None,
            link_target: None,
            error: None,
        }
    }

    /// 存在且不是目录。
    ///
    /// **它不排除 App Execution Alias** —— `WindowsApps\python.exe` 就是这么一根
    /// 0 字节、`exists == true`、`is_dir == false` 的东西。先看 tag 是调用方的责任，
    /// 而且这正是 `alias-ghost` 独立成层的原因。
    #[must_use]
    pub const fn is_file(&self) -> bool {
        self.exists && !self.is_dir
    }
}

/// 目录里的一个条目。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntryFacts {
    /// 名字（单层，不含路径）。
    pub name: String,
    /// 是目录（跟随失败时为 `false`）。
    pub is_dir: bool,
    /// reparse 形状。
    pub reparse: ReparseKind,
}

/// 读文件系统事实的抽象。
///
/// 真实实现读机器；测试实现读固定装置（[`crate::fixture::FakeFileSystem`]）。
/// **两个实现都只有读方法** —— "这一票只读"在这里是类型事实，不是纪律。
pub trait FileSystem {
    /// 一个路径的完整事实。**不存在的路径不返回错误**，返回 `exists == false`
    /// （调用方绝大多数时候要的是"有没有"，不是"为什么没有"）。
    fn inspect(&self, path: &Path) -> FileFacts;

    /// 列出目录内容。目录不存在或没有权限时返回**空列表** ——
    /// 这与 Windows 自己的 `PATH` 查找在这两种情况下的行为一致。
    fn list_dir(&self, path: &Path) -> Vec<DirEntryFacts>;
}

/// 真实实现：`FindFirstFileW` + `std::fs::read_dir`。
///
/// 无状态，所以是个零大小类型。
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFileSystem;

impl RealFileSystem {
    /// 链接目标。**只对真的链接读**：对 App Execution Alias 调 `read_link` 没有意义
    /// （它没有目标路径），而 Rust 的 `read_link` 在 Windows 上能正确读出 junction 的
    /// 替代名（本机 `java8path` → `java8path_target_1783390` 就是这么读出来的）。
    fn link_target(path: &Path, reparse: ReparseKind) -> Option<String> {
        if !reparse.is_link() {
            return None;
        }
        std::fs::read_link(path)
            .ok()
            .map(|target| target.to_string_lossy().into_owned())
    }
}

impl FileSystem for RealFileSystem {
    fn inspect(&self, path: &Path) -> FileFacts {
        match sys::find_first(path) {
            Ok(found) => {
                let reparse = found.reparse_tag.map_or(ReparseKind::None, |tag| {
                    ReparseKind::from_tag(tag, found.is_dir)
                });
                FileFacts {
                    path: path.to_path_buf(),
                    exists: true,
                    is_dir: found.is_dir,
                    size: found.size,
                    reparse,
                    link_target: Self::link_target(path, reparse),
                    error: None,
                }
            }
            Err(code) if code == sys::ERROR_FILE_NOT_FOUND || code == sys::ERROR_PATH_NOT_FOUND => {
                FileFacts::missing(path)
            }
            Err(code) => FileFacts {
                error: Some(PlatformError::from_win32(code, path.to_string_lossy())),
                ..FileFacts::missing(path)
            },
        }
    }

    fn list_dir(&self, path: &Path) -> Vec<DirEntryFacts> {
        let Ok(entries) = std::fs::read_dir(path) else {
            return Vec::new();
        };
        let mut out: Vec<DirEntryFacts> = entries
            .flatten()
            .map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let facts = self.inspect(&path.join(&name));
                DirEntryFacts {
                    name,
                    is_dir: facts.is_dir,
                    reparse: facts.reparse,
                }
            })
            .collect();
        // 排序让上层输出与迭代顺序都确定 —— `--json` 必须逐字节稳定（决策 35），
        // 而 `read_dir` 的顺序在 NTFS 上不保证。
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_the_win32_values() {
        assert_eq!(IO_REPARSE_TAG_MOUNT_POINT, 0xa000_0003);
        assert_eq!(IO_REPARSE_TAG_SYMLINK, 0xa000_000c);
        assert_eq!(IO_REPARSE_TAG_APPEXECLINK, 0x8000_001b);
    }

    #[test]
    fn app_exec_alias_is_its_own_thing_and_is_not_a_link() {
        let alias = ReparseKind::from_tag(IO_REPARSE_TAG_APPEXECLINK, false);
        assert_eq!(alias, ReparseKind::AppExecAlias);
        assert!(alias.is_app_exec_alias());
        assert!(!alias.is_link(), "别名不是重定向，没有目标路径可读");
    }

    #[test]
    fn symlink_tag_needs_the_directory_bit_to_be_disambiguated() {
        assert_eq!(
            ReparseKind::from_tag(IO_REPARSE_TAG_SYMLINK, true),
            ReparseKind::SymlinkDir
        );
        assert_eq!(
            ReparseKind::from_tag(IO_REPARSE_TAG_SYMLINK, false),
            ReparseKind::SymlinkFile
        );
    }

    #[test]
    fn junction_is_a_link_but_not_an_alias() {
        let junction = ReparseKind::from_tag(IO_REPARSE_TAG_MOUNT_POINT, true);
        assert_eq!(junction, ReparseKind::Junction);
        assert!(junction.is_link());
        assert!(!junction.is_app_exec_alias());
    }

    #[test]
    fn unknown_tags_are_recorded_not_guessed() {
        let other = ReparseKind::from_tag(0x9000_1234, false);
        assert_eq!(other, ReparseKind::Other(0x9000_1234));
        assert_eq!(other.tag(), Some(0x9000_1234));
        assert!(!other.is_link());
    }

    #[test]
    fn display_and_parse_round_trip_for_the_fixture_format() {
        for kind in [
            ReparseKind::None,
            ReparseKind::Junction,
            ReparseKind::SymlinkFile,
            ReparseKind::SymlinkDir,
            ReparseKind::AppExecAlias,
            ReparseKind::Other(0x8000_001b),
        ] {
            let text = kind.to_string();
            assert_eq!(text.parse::<ReparseKind>(), Ok(kind), "往返失败：{text}");
        }
        assert_eq!("app-exec-alias".parse(), Ok(ReparseKind::AppExecAlias));
        assert_eq!("other:27".parse(), Ok(ReparseKind::Other(27)));
        assert!("不是形状".parse::<ReparseKind>().is_err());
    }

    #[test]
    fn a_zero_byte_alias_is_a_file_by_the_low_level_predicate() {
        // 这条测试钉住的是**为什么 `alias-ghost` 必须独立成层**：
        // 只看 `exists` / `is_file` 的话，0 字节别名看起来就是一个存在的文件。
        let facts = FileFacts {
            path: PathBuf::from(r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\python.exe"),
            exists: true,
            is_dir: false,
            size: 0,
            reparse: ReparseKind::AppExecAlias,
            link_target: None,
            error: None,
        };
        assert!(facts.is_file());
        assert_eq!(facts.size, 0);
        assert!(facts.reparse.is_app_exec_alias());
    }

    #[test]
    fn real_file_system_reads_the_tests_own_temp_dir_and_never_the_machine() {
        let dir = crate::test_support::TempDir::new("fs-facts");
        dir.write("plain.txt", b"hello");
        dir.mkdir("sub");
        let fs = RealFileSystem;

        let file = fs.inspect(&dir.join("plain.txt"));
        assert!(file.exists);
        assert!(file.is_file());
        assert_eq!(file.size, 5);
        assert_eq!(file.reparse, ReparseKind::None);
        assert!(file.error.is_none());

        let missing = fs.inspect(&dir.join("nope.txt"));
        assert!(!missing.exists);
        assert!(!missing.is_file());
        assert!(missing.error.is_none(), "不存在不是错误");

        let entries = fs.list_dir(dir.path());
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["plain.txt", "sub"], "必须排序且只有一个层级");
        assert!(
            entries
                .iter()
                .find(|e| e.name == "sub")
                .is_some_and(|e| e.is_dir)
        );
    }

    #[test]
    fn listing_a_missing_directory_is_empty_not_a_panic() {
        let dir = crate::test_support::TempDir::new("fs-facts-missing");
        assert!(RealFileSystem.list_dir(&dir.join("nope")).is_empty());
    }
}
