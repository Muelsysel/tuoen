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
use std::io::Read;
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

/// 一次**有界**读取的结果。
///
/// # 为什么是四态而不是 `Option<Vec<u8>>`
///
/// `NotFound`（真的不存在）与 `Unreadable`（存在，但读不了 —— 目录、权限、被占用）
/// 是**不同的两件事**，而 `TooLarge` 是第三种："存在、也读得了，但我们不愿意把它
/// 读进内存"。合并成 `Option` 会让"文件太大"与"没有这个文件"长得一模一样，
/// 而调用方要做的下一步完全不同：决策 179 要求跳过的每一样东西都在 `skipped.toml`
/// 里带着**具体**原因（`too-large` / `unreadable`），而不是一句"读不到"。
///
/// # 没有 `Serialize`，这是故意的
///
/// [`ReadOutcome::Unreadable`] 的 `message` 是**本地化**的系统错误文本（本机 zh-CN），
/// 它是给人看的一句话。不给这个类型任何 `Serialize` 实现，就没有"顺手把它塞进
/// `--json`"的入口 —— 而决策 152 要求成功载荷纯 ASCII。要进快照的只有
/// `size` / 稳定 slug 这类数据，那是调用方自己拼的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// 读到了，**最多 `limit` 字节**（文件本身不超过 `limit`，所以这就是全部内容）。
    Bytes(Vec<u8>),
    /// 这个路径不存在。**只有真的不存在才是它** —— 读不了的目录不是 `NotFound`。
    NotFound,
    /// 存在，但比 `limit` 大，**我们没有读完**。
    ///
    /// `size` 是磁盘上**真实的**字节数，不是 `limit`：调用方要拿它去
    /// `skipped.toml` 里说"这个文件 8 GB，太大了"，印一个 `limit` 只会让人以为
    /// 文件正好是那个大小。
    TooLarge {
        /// 真实的字节数（不是 `limit`）。
        size: u64,
    },
    /// 存在，但读不了。`message` 是系统错误文本，**只进日志与跳过原因**。
    Unreadable {
        /// 系统错误文本（本地化）。**不进 `--json` 成功载荷。**
        message: String,
    },
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

    /// 读一个文件，**最多 `limit` 字节**。
    ///
    /// **超过 `limit` 时不许"读完再判"**：调用方会去读用户的配置文件
    /// （`.npmrc` / `.m2/settings.xml` / `.gitconfig`，决策 176/178），而把一个 8 GB 的
    /// 文件读进内存不是"慢"，是**把整机拖垮**（与 `AGENTS.md` 的进程铁律同源的
    /// 代价不对称：一个字节的差错换整机失去响应）。
    ///
    /// # 没有默认实现，这是故意的
    ///
    /// 一个默认返回 [`ReadOutcome::Unreadable`] 的实现，会把"**某个实现忘了实现它**"
    /// 变成"这个文件读不了"—— 一句看起来完全合理的假话。更糟的是它的后果：
    /// 读不到内容的配置文件不会被扫凭据形状（决策 178），于是**一个含 PAT 的
    /// `settings.xml` 会以"看起来被捕获了"的姿态进快照**。所以每个实现者都必须
    /// 显式回答这个问题 —— 编译器会把"忘了"变成一条错误，而不是一句谎话。
    fn read(&self, path: &Path, limit: u64) -> ReadOutcome;
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

    /// 有界读取。**先看元数据，再打开文件** —— 这个顺序是有意的。
    ///
    /// 三条最常见的答案（不存在 / 是目录 / 太大）都能在**不打开文件**的情况下答出来，
    /// 而"太大"尤其重要：一个 8 GB 的文件在"先读后判"的实现里会被完整读进内存，
    /// 换来的只是一个 `TooLarge`。
    fn read(&self, path: &Path, limit: u64) -> ReadOutcome {
        // ① 元数据。`NotFound` 只给真的不存在 —— 权限问题、路径太长、盘符不在，
        //    都是 `Unreadable`（"读不了"与"没有"是两件事，见 `ReadOutcome` 的文档）。
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::NotFound,
            Err(err) => {
                return ReadOutcome::Unreadable {
                    message: err.to_string(),
                };
            }
        };
        if metadata.is_dir() {
            return ReadOutcome::Unreadable {
                message: "是一个目录，不是一个文件".to_owned(),
            };
        }
        if metadata.len() > limit {
            return ReadOutcome::TooLarge {
                size: metadata.len(),
            };
        }

        // ② 真的读，但只读 `limit + 1` 字节：多出来的那一个字节就是
        //    "它比 limit 大"的证据，而 `limit + 1` 是一个**有界**的分配。
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            // 文件在①与②之间被删掉了 —— 那仍然是"不存在"，不是"读不了"。
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return ReadOutcome::NotFound,
            Err(err) => {
                return ReadOutcome::Unreadable {
                    message: err.to_string(),
                };
            }
        };
        let mut bytes = Vec::new();
        if let Err(err) = file.take(limit.saturating_add(1)).read_to_end(&mut bytes) {
            return ReadOutcome::Unreadable {
                message: err.to_string(),
            };
        }
        if bytes.len() as u64 > limit {
            // 走到这里只有一种可能：文件在①与②之间**长大了**（元数据已经 > limit
            // 的情况在上面就返回了）。真实的字节数只有磁盘知道 —— 再问一次；
            // 问不到（或者它又缩回去了）就报"我至少读到了这么多"，那仍然是一句真话，
            // 而且它**不会**小于等于 `limit`。
            let observed = bytes.len() as u64;
            let size = std::fs::metadata(path).map_or(observed, |meta| meta.len().max(observed));
            return ReadOutcome::TooLarge { size };
        }
        ReadOutcome::Bytes(bytes)
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

    #[test]
    fn read_answers_the_four_states_and_only_the_temp_dir_is_touched() {
        let dir = crate::test_support::TempDir::new("fs-read-states");
        dir.write("small.txt", b"hello");
        dir.mkdir("sub");
        let fs = RealFileSystem;

        // ① Bytes —— 文件比 limit 小的时候，读到的就是**全部**内容。
        assert_eq!(
            fs.read(&dir.join("small.txt"), 4096),
            ReadOutcome::Bytes(b"hello".to_vec())
        );

        // ② NotFound —— 只有真的不存在才是它。
        assert_eq!(fs.read(&dir.join("nope.txt"), 4096), ReadOutcome::NotFound);
        assert_eq!(
            fs.read(&dir.join(r"no-such-dir\x.txt"), 4096),
            ReadOutcome::NotFound,
            "父目录不存在也是 NotFound（不是 Unreadable）"
        );

        // ③ TooLarge —— `size` 是**磁盘上真实的字节数**，不是 limit。
        assert_eq!(
            fs.read(&dir.join("small.txt"), 4),
            ReadOutcome::TooLarge { size: 5 },
            "报 limit 会让人以为文件正好是 4 字节"
        );

        // ④ Unreadable —— 存在、但不是我们能读的文件。
        match fs.read(dir.path(), 4096) {
            ReadOutcome::Unreadable { message } => {
                assert!(!message.is_empty(), "必须说得出为什么：{message}");
            }
            other => panic!("目录必须是 Unreadable（不是 NotFound），实际 {other:?}"),
        }
    }

    #[test]
    fn the_read_boundary_is_the_file_size_and_not_the_limit() {
        let dir = crate::test_support::TempDir::new("fs-read-boundary");
        dir.write("exactly.txt", b"12345");
        dir.write("one-more.txt", b"123456");
        let fs = RealFileSystem;

        // 恰好 limit：读得到。这是 `take(limit + 1)` 的**下**边界。
        assert_eq!(
            fs.read(&dir.join("exactly.txt"), 5),
            ReadOutcome::Bytes(b"12345".to_vec())
        );
        // limit + 1：读不到，而 `size` 报的是 6。
        assert_eq!(
            fs.read(&dir.join("one-more.txt"), 5),
            ReadOutcome::TooLarge { size: 6 }
        );
        // 同一条文件换个 limit 就变成 Bytes —— 边界跟着 limit 走，不跟着文件走。
        assert_eq!(
            fs.read(&dir.join("one-more.txt"), 6),
            ReadOutcome::Bytes(b"123456".to_vec())
        );
        // limit = 0：任何非空文件都是 TooLarge，而空文件读得到空内容。
        dir.write("empty.txt", b"");
        assert_eq!(
            fs.read(&dir.join("empty.txt"), 0),
            ReadOutcome::Bytes(Vec::new())
        );
        assert_eq!(
            fs.read(&dir.join("exactly.txt"), 0),
            ReadOutcome::TooLarge { size: 5 }
        );
    }

    /// 钉住"**元数据先于打开**"：`TooLarge` 来自元数据，不是来自一次成功的读取。
    ///
    /// **唯一的证据形状**：一个"先读后判"的实现在**正常**文件上给出的结果与"先看元数据"
    /// 完全相同（同样的 `TooLarge { size }`），所以拿正常文件写断言是**没有牙齿**的。
    /// 只有"元数据看得见、但打不开"的文件能分辨两条路径：先看元数据 → `TooLarge`，
    /// 先读 → `Unreadable`。
    ///
    /// # 它隐含的假设（以及假设不成立时该怎么办）
    ///
    /// 它假设 `std::fs::metadata`（Windows 上是 `GetFileAttributesExW`）**不做共享检查**，
    /// 于是独占打开（`share_mode(0)`）的文件正好是"元数据看得见、`File::open` 打不开"。
    /// 这一点**在本机（Win11 / NTFS）实测成立**；我**没有**在别的 Windows 版本或非 NTFS
    /// 卷上验证过它。
    ///
    /// **若不成立，红的是用例而不是产品** —— 那时应当把它改成 `#[ignore]` 并在这里写明
    /// 理由，**不许删掉**：删掉就等于把"我们曾经用一条会失败的测量盯过这件事"这个事实
    /// 一起删了（`AGENTS.md`：一条不能失败的测量不是测量）。
    ///
    /// # 它**没有**证明的事
    ///
    /// "8 GB 的文件不会被读进内存"。它证明的是"**没有打开文件**"，不是"最多只分配了
    /// `limit + 1` 字节" —— 后者没有任何用例在盯（诚实未覆盖）。
    #[test]
    fn too_large_is_decided_from_metadata_before_the_file_is_ever_opened() {
        // **这条用例是"不许读完再判"唯一的证据形状。**
        //
        // 一个"先读后判"的实现在**正常**文件上给出的结果与"先看元数据"完全相同
        // （同样是 `TooLarge { size }`），所以拿正常文件写断言是**没有牙齿**的。
        // 唯一能分辨两条路径的是一个"元数据看得见、但打不开"的文件：
        //   · 先看元数据 → `TooLarge`（大小在元数据里，根本不需要打开）；
        //   · 先读       → `Unreadable`（打开就失败）。
        // 独占打开（`share_mode(0)`）就是那个文件 —— `File::open` 自带
        // `FILE_SHARE_READ|WRITE|DELETE`，所以用默认参数造不出这个形状。
        // 用 `OpenOptionsExt::share_mode` 而不是 `CreateFileW`：本仓库只允许在 `sys.rs`
        // 里写 unsafe（`unsafe_code = "deny"` + 那一条 `allow` 的理由是"全仓库唯一"）。
        let dir = crate::test_support::TempDir::new("fs-read-no-open");
        dir.write("locked.txt", b"123456");
        let locked = open_exclusively(&dir.join("locked.txt"));

        assert_eq!(
            RealFileSystem.read(&dir.join("locked.txt"), 5),
            ReadOutcome::TooLarge { size: 6 },
            "锁着的文件也答得出 TooLarge，说明它来自元数据而不是一次成功的读取"
        );
        // 同一把锁下，一个 limit 足够大的读**必须**失败：否则上面那条就可能是
        // "读取恰好成功"而不是"根本没读"。
        match RealFileSystem.read(&dir.join("locked.txt"), 4096) {
            ReadOutcome::Unreadable { message } => assert!(!message.is_empty()),
            other => panic!("独占锁下不该读得出来，实际 {other:?}"),
        }

        // 句柄在 `dir` 之前析构（声明顺序相反），所以临时目录删得掉。
        drop(locked);
        assert_eq!(
            RealFileSystem.read(&dir.join("locked.txt"), 4096),
            ReadOutcome::Bytes(b"123456".to_vec()),
            "锁一放就又能读了 —— 上面那条 `Unreadable` 确实来自锁，不是别的原因"
        );
    }

    /// 以**独占**方式（`dwShareMode = 0`）打开一个文件，句柄 `Drop` 时关闭。
    ///
    /// 只在这条用例里用：它造出"元数据看得见、但 `File::open` 打不开"的文件，
    /// 而那是分辨"先看元数据"与"先读"的唯一形状。
    fn open_exclusively(path: &Path) -> std::fs::File {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .expect("独占打开失败 —— 这条用例的形状就没了，不能静默通过")
    }
}
