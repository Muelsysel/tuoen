//! 归档解压的错误。
//!
//! ## 为什么每一条违规都要有名字
//!
//! 票据 #5 的验收里写着："拒绝时给出**具体原因**（哪一种违规），因为
//! '解压失败'不足以判断是上游问题还是攻击。"
//!
//! 这句话是有分量的：一个下载失败的归档与一个**精心构造的**归档，
//! 对用户的处置完全不同 —— 前者重试，后者要报告。所以这里没有
//! 一个笼统的 `InvalidArchive`，而是每一条规则一个变体。
//!
//! 判据来自 [`crate::name`]，实测依据见 `research/BSDTAR_SAFETY_MEASURED.md`。

use std::path::PathBuf;

use thiserror::Error;

/// 条目名字违反了哪一条规则。
///
/// 变体的**顺序**是检查顺序，也是有意义的：先看是不是绝对路径，
/// 再看有没有 `..`，最后才看名字本身的形态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum NameViolation {
    /// 空名字，或者只剩分隔符。
    Empty,
    /// 绝对路径（`/x`、`C:/x`、`C:\x`）。
    ///
    /// **实测**：bsdtar 会把前导 `/` 静默剥掉（警告 `Removing leading '/'
    /// from member names`），所以它不会逃逸 —— 但它说明这个归档不是
    /// 一个正经的归档，而且换个解压器就可能逃逸。
    AbsolutePath,
    /// UNC 路径（`//server/share/x`、`\\server\share\x`）。
    UncPath,
    /// 带盘符（`C:x`、`C:/x`）。
    DriveLetter,
    /// 含 `..` 段（`../x`、`a/../../x`、`..\x`）。
    ParentTraversal,
    /// 保留设备名：`CON` / `PRN` / `AUX` / `NUL` / `COM1-9` / `LPT1-9`，
    /// 以及它们的 `COM¹` 上标形式。
    ///
    /// **实测**：bsdtar **真的会创建**这些文件（用 `\\?\` 绕过 Win32 的
    /// 设备名解析），而创建出来的文件**普通路径看不见**
    /// （`Test-Path '…\NUL'` = False，`Test-Path -LiteralPath '\\?\…\NUL'` = True）
    /// —— 清不掉的残骸。
    ReservedDeviceName,
    /// 结尾是点或空格（`trailing.`、`trailing `）。
    ///
    /// **实测**：bsdtar 真的创建了它们，而 Win32 的路径规范化会吃掉结尾的
    /// 点与空格 —— 于是这些文件在资源管理器与多数工具里**打不开、删不掉**。
    TrailingDotOrSpace,
    /// 名字里有 `:`（ADS 写原语：`file.txt:evil`）。
    ///
    /// **实测**：bsdtar 把它**静默改名**成 `file.txt_evil`（没写进 ADS，
    /// 但名字变了而没有任何提示）。
    AlternateDataStream,
    /// 名字里有控制字符（`< 0x20` 或 `0x7F`）。
    ///
    /// **实测**：bsdtar 在列表里把它们转义成 `\n` / `\r`，所以列表看不出
    /// 真相；而 `-tf` 的转义是**双向的** —— 真目录 `bin` + 文件 `node.exe`
    /// 与"名字里含换行的 `bin<LF>ode.exe`"长得一样。
    ControlCharacter,
    /// 路径里某一段是空的（`a//b`）或是 `.`（`a/./b`）。
    ///
    /// 这两者本身不逃逸，但它们让"这个名字等于那个名字"变得不确定，
    /// 而大小写碰撞检测依赖名字的唯一性。
    EmptyOrDotSegment,
    /// 路径太深。
    TooDeep,
    /// 单个路径太长（超过 [`crate::limits::MAX_RELATIVE_PATH`]）。
    TooLong,
    /// 归档里的条目数超过上限。
    TooManyEntries,
    /// 累计解压体积超过上限（zip bomb）。
    TooLarge,
    /// 一个条目声明的大小超过单文件上限。
    EntryTooLarge,
}

impl NameViolation {
    /// 稳定取值，进 `--json`，**不本地化**。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::AbsolutePath => "absolute-path",
            Self::UncPath => "unc-path",
            Self::DriveLetter => "drive-letter",
            Self::ParentTraversal => "parent-traversal",
            Self::ReservedDeviceName => "reserved-device-name",
            Self::TrailingDotOrSpace => "trailing-dot-or-space",
            Self::AlternateDataStream => "alternate-data-stream",
            Self::ControlCharacter => "control-character",
            Self::EmptyOrDotSegment => "empty-or-dot-segment",
            Self::TooDeep => "too-deep",
            Self::TooLong => "too-long",
            Self::TooManyEntries => "too-many-entries",
            Self::TooLarge => "too-large",
            Self::EntryTooLarge => "entry-too-large",
        }
    }

    /// 给用户的中文解释。
    ///
    /// **不是** `as_str` 的翻译 —— 它要回答"这为什么危险"，
    /// 而不只是"违反了哪一条"。
    #[must_use]
    pub const fn why(self) -> &'static str {
        match self {
            Self::Empty => "条目名是空的",
            Self::AbsolutePath => "条目名是绝对路径（前导 /）",
            Self::UncPath => "条目名是 UNC 网络路径",
            Self::DriveLetter => "条目名带盘符",
            Self::ParentTraversal => "条目名里有 `..`，会写到解压目录之外",
            Self::ReservedDeviceName => {
                "条目名是 Windows 保留设备名（CON/PRN/AUX/NUL/COM1-9/LPT1-9）—— \
                 这类文件会被创建出来，但普通路径看不见也删不掉"
            }
            Self::TrailingDotOrSpace => {
                "条目名以点或空格结尾 —— Win32 会吃掉结尾的点与空格，\
                 于是文件在多数工具里打不开也删不掉"
            }
            Self::AlternateDataStream => "条目名里有 `:`（NTFS 备用数据流的写原语）",
            Self::ControlCharacter => "条目名里有控制字符（会破坏列表与日志的行结构）",
            Self::EmptyOrDotSegment => "条目名里有空的或 `.` 路径段",
            Self::TooDeep => "目录层数太深",
            Self::TooLong => "单个路径太长",
            Self::TooManyEntries => "归档里的条目数超过上限",
            Self::TooLarge => "解压后的总体积超过上限（可能是压缩炸弹）",
            Self::EntryTooLarge => "单个条目声明的大小超过上限",
        }
    }
}

impl std::fmt::Display for NameViolation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}（{}）", self.why(), self.as_str())
    }
}

/// 解压层的失败。
#[derive(Debug, Error)]
pub enum ArchiveError {
    /// 归档里有一个条目名违规。**这是本 crate 最重要的一条错误。**
    #[error("归档里的条目 `{entry}` 不能解压：{violation}")]
    UnsafeEntry {
        /// 归档里的原始名字（原样，不净化 —— 净化过的名字会掩盖证据）。
        entry: String,
        /// 违反了哪一条。
        violation: NameViolation,
    },

    /// 归档里的条目名不是合法的 UTF-8。
    ///
    /// 单独一条而不是并进 [`Self::UnsafeEntry`]：因为**没法把它显示出来**
    /// 而显示出来的都是猜的，而"猜出来的名字"正是我们不想要的。
    #[error("归档里的第 {index} 个条目名不是合法的 UTF-8：{bytes:02x?}")]
    NonUtf8Entry {
        /// 它是第几个条目（从 1 开始）。
        index: usize,
        /// 原始字节（截断到 64 字节，够看出意图）。
        bytes: Vec<u8>,
    },

    /// 归档里有非普通文件的条目（symlink / 硬链接 / 设备 / fifo）。
    #[error(
        "归档里有非普通文件的条目（symlink / 硬链接 / 设备 / fifo）：{kind}\n\
         实测：在 Windows 上 bsdtar 会拒绝创建它们并让退出码变成 1 —— \
         于是解压结果是**半个目录**。宁可整个拒绝，也不留下半个版本。"
    )]
    NonRegularEntry {
        /// `-tvf` 首字符（`l` / `h` / `p` / `c` / `b` / `s`）。
        kind: String,
    },

    /// 归档里两个条目只差大小写。
    ///
    /// **实测**：NTFS 默认大小写不敏感，于是 bsdtar **静默覆盖**
    /// （`Readme.txt` + `README.TXT` 只剩一个，内容是后一个）。
    /// 安装场景里这意味着归档可以偷偷替换掉自己的合法文件。
    #[error(
        "归档里有两个条目只差大小写（`{first}` 与 `{second}`）—— \
         在 NTFS 上第二个会**静默覆盖**第一个"
    )]
    CaseCollision {
        /// 先出现的那个。
        first: String,
        /// 与它只差大小写的那个。
        second: String,
    },

    /// 解压器不存在或跑不起来。
    ///
    /// `reason` 是 `ProcessOutcome::spawn_error` 的原话 —— **只用于显示**，
    /// 判断逻辑不看它（`AGENTS.md`：永远不要匹配错误文本）。
    #[error("找不到解压器，或者它起不来：`{program}`（{reason}）")]
    ExtractorUnavailable {
        /// 找的程序名。
        program: String,
        /// 起不来的原因（人读）。
        reason: String,
    },

    /// 解压器报了非零退出码 —— **有东西被它拒了，所以结果是半个目录**。
    #[error(
        "解压 `{archive}` 失败（{program} 退出码 {code}）—— \
         非零退出码意味着有条目被拒绝，磁盘上留下的是**半个目录**，\
         已清理。解压器的原话：\n{stderr}"
    )]
    ExtractorFailed {
        /// 归档路径。
        archive: PathBuf,
        /// 解压器路径。
        program: PathBuf,
        /// 退出码。
        code: i32,
        /// 解压器的 stderr（它会点名是哪个条目、为什么）。
        stderr: String,
    },

    /// 解压器超时。
    #[error("解压 `{archive}` 超时（超过 {seconds} 秒）")]
    ExtractorTimedOut {
        /// 归档路径。
        archive: PathBuf,
        /// 超时秒数。
        seconds: u64,
    },

    /// **解压后**审计发现目录里有 reparse point。
    ///
    /// 这一层是权威判据：条目列表是**有损**的（bsdtar 会转义控制字符），
    /// 所以"列表里没有 symlink"不等于"磁盘上没有 symlink"。
    #[error("解压结果里有 reparse point（{kind}）：{path}")]
    ReparsePointInResult {
        /// 落盘的路径。
        path: PathBuf,
        /// 是哪种 reparse。
        kind: String,
    },

    /// **解压后**审计发现目录里有普通路径看不见的名字。
    #[error(
        "解压结果里有普通路径看不见的文件：{path}\n\
         它存在（用 \\\\?\\ 前缀能查到）但普通路径访问不到 —— \
         这正是保留设备名造成的残骸，删都删不干净。"
    )]
    InvisibleFile {
        /// 落盘的路径。
        path: PathBuf,
    },

    /// 审计在磁盘上发现了名字违规。
    ///
    /// 与 [`Self::UnsafeEntry`] 的区别：那条是**解压前**从列表里看出来的，
    /// 这条是**解压后**从磁盘上看到的。后者是权威的 —— 实测 bsdtar 会把
    /// `:` 静默改成 `_`，所以磁盘上的名字可能与列表里的不同。
    #[error("解压结果里有违规的名字 `{path}`：{violation}")]
    UnsafeResultPath {
        /// 落盘的路径。
        path: PathBuf,
        /// 违反了哪一条。
        violation: NameViolation,
    },

    /// 目标版本目录已经存在。
    #[error("目标目录已经存在，不覆盖：{path}")]
    DestinationExists {
        /// 目标路径。
        path: PathBuf,
    },

    /// 落盘。
    #[error("{operation} `{path}` 失败：{source}")]
    Io {
        /// 在做什么（中文，给用户看）。
        operation: String,
        /// 路径。
        path: PathBuf,
        /// 底层原因。
        source: std::io::Error,
    },

    /// 归档格式不认识。
    #[error("不认识的归档格式：{path}（扩展名与文件头都不像我们支持的格式）")]
    UnknownFormat {
        /// 归档路径。
        path: PathBuf,
    },
}

impl ArchiveError {
    /// 稳定分类，进 `--json`，**不本地化**。
    ///
    /// 给脚本与 GUI 用；中文解释在 `Display` 里。
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::UnsafeEntry { .. } => "unsafe-entry",
            Self::NonUtf8Entry { .. } => "non-utf8-entry",
            Self::NonRegularEntry { .. } => "non-regular-entry",
            Self::CaseCollision { .. } => "case-collision",
            Self::ExtractorUnavailable { .. } => "extractor-unavailable",
            Self::ExtractorFailed { .. } => "extractor-failed",
            Self::ExtractorTimedOut { .. } => "extractor-timed-out",
            Self::ReparsePointInResult { .. } => "reparse-point-in-result",
            Self::InvisibleFile { .. } => "invisible-file",
            Self::UnsafeResultPath { .. } => "unsafe-result-path",
            Self::DestinationExists { .. } => "destination-exists",
            Self::Io { .. } => "io",
            Self::UnknownFormat { .. } => "unknown-format",
        }
    }

    /// 这是不是"这个归档本身有问题"（而不是环境问题）。
    ///
    /// 调用方用它决定要不要向用户报告 —— 一个**构造过的**归档值得报告，
    /// 一个找不到 `tar.exe` 的环境问题不值得。
    #[must_use]
    pub const fn is_archive_at_fault(&self) -> bool {
        matches!(
            self,
            Self::UnsafeEntry { .. }
                | Self::NonUtf8Entry { .. }
                | Self::NonRegularEntry { .. }
                | Self::CaseCollision { .. }
                | Self::ReparsePointInResult { .. }
                | Self::InvisibleFile { .. }
                | Self::UnsafeResultPath { .. }
                | Self::UnknownFormat { .. }
        )
    }

    /// 违反了哪一条名字规则（如果有）。
    #[must_use]
    pub const fn name_violation(&self) -> Option<NameViolation> {
        match self {
            Self::UnsafeEntry { violation, .. } | Self::UnsafeResultPath { violation, .. } => {
                Some(*violation)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_violation_has_a_stable_slug_and_a_distinct_explanation() {
        // 这两件事都必须成立，而且理由不同：
        // `as_str` 进 `--json`，所以它**不能变**、不能本地化；
        // `why` 给用户看，所以它必须解释"为什么危险"。
        // 两者相同就意味着有人把其中一个当另一个用了。
        let all = [
            NameViolation::Empty,
            NameViolation::AbsolutePath,
            NameViolation::UncPath,
            NameViolation::DriveLetter,
            NameViolation::ParentTraversal,
            NameViolation::ReservedDeviceName,
            NameViolation::TrailingDotOrSpace,
            NameViolation::AlternateDataStream,
            NameViolation::ControlCharacter,
            NameViolation::EmptyOrDotSegment,
            NameViolation::TooDeep,
            NameViolation::TooLong,
            NameViolation::TooManyEntries,
            NameViolation::TooLarge,
            NameViolation::EntryTooLarge,
        ];
        let mut slugs: Vec<&str> = all.iter().map(|v| v.as_str()).collect();
        slugs.sort_unstable();
        let count = slugs.len();
        slugs.dedup();
        assert_eq!(slugs.len(), count, "两个变体共用了同一个 slug");

        for violation in all {
            assert!(
                !violation.as_str().contains(char::is_uppercase),
                "slug 必须是小写 kebab-case：{}",
                violation.as_str()
            );
            assert!(
                violation.as_str().is_ascii(),
                "slug 必须是 ASCII：{}",
                violation.as_str()
            );
            assert_ne!(
                violation.why(),
                violation.as_str(),
                "why() 不是 slug 的翻译，它要解释为什么危险"
            );
        }
    }

    #[test]
    fn archive_at_fault_separates_upstream_problems_from_environment_problems() {
        // 这个区分是给人看的：一个构造过的归档值得报告，
        // 一个找不到 tar.exe 的环境问题不值得。
        let unsafe_entry = ArchiveError::UnsafeEntry {
            entry: "../x".to_owned(),
            violation: NameViolation::ParentTraversal,
        };
        assert!(unsafe_entry.is_archive_at_fault());
        assert_eq!(
            unsafe_entry.name_violation(),
            Some(NameViolation::ParentTraversal)
        );

        let io = ArchiveError::Io {
            operation: "写".to_owned(),
            path: PathBuf::from("C:/x"),
            source: std::io::Error::other("boom"),
        };
        assert!(!io.is_archive_at_fault());
        assert_eq!(io.name_violation(), None);
    }
}
