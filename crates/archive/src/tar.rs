//! 调系统 `tar.exe`（bsdtar / libarchive）读归档与解压。
//!
//! ## 为什么 shell out 到 `tar.exe`
//!
//! 与决策 39（shell out 到系统 `curl.exe`）同一条理由：**系统已经带了
//! 一个能用的、比我们更懂格式的实现**，而自己实现 zip + 5 种 tar 压缩
//! 等于把攻击面从"我们的校验逻辑"扩大到"我们的格式解析器"。
//!
//! 实测依据（`research/BSDTAR_SAFETY_MEASURED.md`）：本机
//! `C:\Windows\System32\tar.exe` = bsdtar 3.8.8 / libarchive 3.8.8，
//! 它**能**读 `.zip` / `.tar.gz` / `.tar.xz` / `.tar.zst` / `.tar.bz2`
//! **以及 `.7z`**（实测解出 node 的 2454 个条目、`node.exe` 能跑）。
//!
//! ## 它的两道防线，以及为什么不够
//!
//! 实测它**挡住了**：`..`（各种拼法）、symlink（连指向根内的也拒）、
//! 硬链接、fifo、字符设备。而且**只要跳过了任何东西，退出码就是 1**。
//!
//! 实测它**没挡住**：保留设备名（`CON` 真的被创建，且普通路径看不见）、
//! 结尾的点与空格、`:`（静默改名成 `_`）、大小写碰撞（静默覆盖）。
//!
//! 所以这里的定位是**第二层**：它负责"把字节展开"，我们负责"拒绝危险的
//! 名字"与"审计落盘结果"。
//!
//! ## 两次列表遍历的代价（诚实记下）
//!
//! 名字从 `-tf` 拿，条目类型从 `-tvf` 拿（首字符：`-` 文件 / `d` 目录 /
//! `l` symlink / `h` 硬链接 / `p` fifo / `c` 字符设备）。**两条命令都要跑**，
//! 因为**没有任何一条 CLI 调用同时给出机器可读的名字与类型**。
//!
//! 对 zip 这不花钱（读中央目录）；对**压缩过的 tar 会解压两遍**
//! （`.tar.gz` 等）。这条代价只落在 tar 家族上，而 Windows 上的制品
//! 绝大多数是 zip/7z —— 所以接受，并写在这里。

use std::path::{Path, PathBuf};
use std::time::Duration;

use tuoen_platform::{ProcessOutcome, ProcessRunner};

use crate::error::ArchiveError;

/// 解压一个真实制品的超时。
///
/// 依据：本机实测 Temurin JDK 那种 ~190 MB 的 `.tar.gz` 解开约 20 秒，
/// node 的 35 MB zip 约 2 秒。给 10 分钟是"再慢的机器也够"，
/// 而不是"刚好够本机"。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(600);

/// 归档里的一个条目，只从列表里知道的那些事实。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedEntry {
    /// 归档里的原始名字（`-tf` 逐字给出的，**未净化**）。
    pub raw_name: String,
}

/// 一次 `-tvf` 里看出来的条目类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// `-`
    File,
    /// `d`
    Directory,
    /// `l` —— symlink。
    Symlink,
    /// `h` —— 硬链接。
    HardLink,
    /// `p` —— fifo。
    Fifo,
    /// `c` / `b` —— 字符设备 / 块设备。
    Device,
    /// `s` —— socket。
    Socket,
    /// 别的（没见过，但**不猜**）。
    Other(char),
}

impl EntryKind {
    /// 从 `-tvf` 一行的**首字符**判定。
    ///
    /// **只看首字符**：实测 `-tvf` 的日期字段是**本地化的**
    /// （本机 zh-CN 下输出成 `1�� 01` 乱码），所以整行不可解析。
    /// 但首字符（模式串的第一个字符）是 libarchive 生成的，不本地化。
    #[must_use]
    pub const fn from_mode_char(c: char) -> Self {
        match c {
            '-' => Self::File,
            'd' => Self::Directory,
            'l' => Self::Symlink,
            'h' => Self::HardLink,
            'p' => Self::Fifo,
            'c' | 'b' => Self::Device,
            's' => Self::Socket,
            other => Self::Other(other),
        }
    }

    /// 是不是普通文件或目录。
    #[must_use]
    pub const fn is_regular(self) -> bool {
        matches!(self, Self::File | Self::Directory)
    }

    /// 稳定取值，进 `--json`。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Directory => "directory",
            Self::Symlink => "symlink",
            Self::HardLink => "hard-link",
            Self::Fifo => "fifo",
            Self::Device => "device",
            Self::Socket => "socket",
            Self::Other(_) => "other",
        }
    }
}

/// `tar.exe` 的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarCli {
    program: PathBuf,
}

impl TarCli {
    /// 找 `tar.exe`：先 `%SystemRoot%\System32\tar.exe`，再退到 `PATH`。
    ///
    /// **优先 System32 而不是 PATH**：PATH 上可能有 Git for Windows 或
    /// MSYS2 带来的另一份 `tar.exe`，而它可能是 GNU tar（`--version` 的
    /// 输出与行为都不同）。系统自带的那份是随 Windows 发布的，版本可控。
    ///
    /// # Errors
    ///
    /// 两处都没有。
    pub fn probe() -> Result<Self, ArchiveError> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(root) = std::env::var_os("SystemRoot") {
            candidates.push(PathBuf::from(root).join("System32").join("tar.exe"));
        }
        candidates.push(PathBuf::from(r"C:\Windows\System32\tar.exe"));

        for candidate in &candidates {
            if candidate.is_file() {
                return Ok(Self {
                    program: candidate.clone(),
                });
            }
        }

        // 退到 PATH。
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let candidate = dir.join("tar.exe");
                if candidate.is_file() {
                    return Ok(Self { program: candidate });
                }
            }
        }

        Err(ArchiveError::ExtractorUnavailable {
            program: "tar.exe".to_owned(),
            reason: "System32 与 PATH 上都没有找到（Windows 10 1803+ 应当自带）".to_owned(),
        })
    }

    /// 指定一个程序路径（测试用）。
    #[must_use]
    pub fn at(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// 程序路径。
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// 列出条目名（`-tf`）。
    ///
    /// # Errors
    ///
    /// 解压器跑不起来、超时、非零退出码。
    pub fn list_names(
        &self,
        runner: &dyn ProcessRunner,
        archive: &Path,
    ) -> Result<Vec<ListedEntry>, ArchiveError> {
        let outcome = self.run(runner, archive, &["-tf"])?;
        let text = String::from_utf8_lossy(&outcome.stdout_bytes).into_owned();
        Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| ListedEntry {
                raw_name: line.to_owned(),
            })
            .collect())
    }

    /// 扫一遍条目类型（`-tvf`），返回**所有非普通文件**的类型。
    ///
    /// 返回空表示"全是文件或目录"。
    ///
    /// **只看每行的首字符。** 实测 `-tvf` 的日期字段是本地化的
    /// （本机 zh-CN 下是 `1�� 01`），所以不要去解析整行 —— 那会在
    /// 另一种区域设置下静默出错。
    ///
    /// # Errors
    ///
    /// 同 [`Self::list_names`]。
    pub fn scan_non_regular_kinds(
        &self,
        runner: &dyn ProcessRunner,
        archive: &Path,
    ) -> Result<Vec<EntryKind>, ArchiveError> {
        let outcome = self.run(runner, archive, &["-tvf"])?;
        let text = String::from_utf8_lossy(&outcome.stdout_bytes).into_owned();
        Ok(text
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| EntryKind::from_mode_char(line.chars().next().unwrap_or('?')))
            .filter(|kind| !kind.is_regular())
            .collect())
    }

    /// 解压到 `dest`（`-xf … -C dest`）。
    ///
    /// **要求退出码为 0。** 实测：只要 bsdtar 跳过了任何东西（`..`、
    /// symlink、硬链接、fifo、设备），退出码就是 1 —— 而那时磁盘上
    /// 留下的是**半个目录**。所以非零退出码一律当失败，由调用方清理。
    ///
    /// # Errors
    ///
    /// 解压器跑不起来、超时、非零退出码（含解压器的 stderr 原话，
    /// 它会点名是哪个条目、为什么）。
    pub fn extract(
        &self,
        runner: &dyn ProcessRunner,
        archive: &Path,
        dest: &Path,
        timeout: Duration,
    ) -> Result<(), ArchiveError> {
        // `-C` 的目录必须先存在，否则 bsdtar 会把它当成"要解压出来的目录"
        // 而在父目录里创建它 —— 那是另一种逃逸面。我们保证它存在。
        std::fs::create_dir_all(dest).map_err(|source| ArchiveError::Io {
            operation: "创建解压目标目录".to_owned(),
            path: dest.to_path_buf(),
            source,
        })?;

        let dest_arg = path_to_cli_arg(dest);
        let outcome = runner.run(
            &self.program,
            &["-xf", &path_to_cli_arg(archive), "-C", &dest_arg],
            timeout,
        );

        if !outcome.spawned {
            return Err(self.did_not_spawn(&outcome));
        }
        if outcome.timed_out {
            return Err(ArchiveError::ExtractorTimedOut {
                archive: archive.to_path_buf(),
                seconds: timeout.as_secs(),
            });
        }
        if outcome.exit_code != Some(0) {
            return Err(ArchiveError::ExtractorFailed {
                archive: archive.to_path_buf(),
                program: self.program.clone(),
                code: outcome.exit_code.unwrap_or(-1),
                stderr: tail(&outcome.stderr, 20),
            });
        }
        Ok(())
    }

    /// 进程没起来 —— 把 `spawn_error` 的原话带出去（**只用于显示**）。
    fn did_not_spawn(&self, outcome: &ProcessOutcome) -> ArchiveError {
        ArchiveError::ExtractorUnavailable {
            program: self.program.display().to_string(),
            reason: outcome
                .spawn_error
                .clone()
                .unwrap_or_else(|| "进程没有起来，也没有给出原因".to_owned()),
        }
    }

    /// 跑一次列表命令。
    fn run(
        &self,
        runner: &dyn ProcessRunner,
        archive: &Path,
        args: &[&str],
    ) -> Result<ProcessOutcome, ArchiveError> {
        let archive_arg = path_to_cli_arg(archive);
        let mut full: Vec<&str> = args.to_vec();
        full.push(&archive_arg);
        let outcome = runner.run(&self.program, &full, DEFAULT_TIMEOUT);

        if !outcome.spawned {
            return Err(self.did_not_spawn(&outcome));
        }
        if outcome.timed_out {
            return Err(ArchiveError::ExtractorTimedOut {
                archive: archive.to_path_buf(),
                seconds: DEFAULT_TIMEOUT.as_secs(),
            });
        }
        if outcome.exit_code != Some(0) {
            return Err(ArchiveError::ExtractorFailed {
                archive: archive.to_path_buf(),
                program: self.program.clone(),
                code: outcome.exit_code.unwrap_or(-1),
                stderr: tail(&outcome.stderr, 20),
            });
        }
        Ok(outcome)
    }
}

/// 把路径转成命令行参数。
///
/// **不加引号**：`ProcessRunner` 的约定是"参数已经是分开的、由实现负责
/// 正确转义"。手动加引号会让路径里真的含引号时出错（Windows 允许文件名
/// 含 `"` 吗？不允许 —— 但含 `&` 与 `^` 是允许的，而那是 cmd 的问题，
/// 不是 CreateProcess 的问题）。
fn path_to_cli_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// 取 stderr 的最后 `n` 行。
///
/// bsdtar 会把"延迟的错误"放在最后（`Error exit delayed from previous
/// errors`），所以**取尾部而不是头部**。
fn tail(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_chars_map_to_the_right_kinds() {
        // 这张表来自实测（`research/BSDTAR_SAFETY_MEASURED.md` §4）：
        // `l` 是 symlink 且带 `-> target`，`h` 是硬链接且带 `link to target`。
        assert_eq!(EntryKind::from_mode_char('-'), EntryKind::File);
        assert_eq!(EntryKind::from_mode_char('d'), EntryKind::Directory);
        assert_eq!(EntryKind::from_mode_char('l'), EntryKind::Symlink);
        assert_eq!(EntryKind::from_mode_char('h'), EntryKind::HardLink);
        assert_eq!(EntryKind::from_mode_char('p'), EntryKind::Fifo);
        assert_eq!(EntryKind::from_mode_char('c'), EntryKind::Device);
        assert_eq!(EntryKind::from_mode_char('b'), EntryKind::Device);
        assert_eq!(EntryKind::from_mode_char('s'), EntryKind::Socket);
        // **不认识的字符不猜**：`Other` 会走到"非普通文件"那一支，
        // 于是归档被拒 —— 这是安全的默认。
        assert_eq!(EntryKind::from_mode_char('?'), EntryKind::Other('?'));
    }

    #[test]
    fn only_files_and_directories_count_as_regular() {
        assert!(EntryKind::File.is_regular());
        assert!(EntryKind::Directory.is_regular());
        for kind in [
            EntryKind::Symlink,
            EntryKind::HardLink,
            EntryKind::Fifo,
            EntryKind::Device,
            EntryKind::Socket,
            EntryKind::Other('?'),
        ] {
            assert!(!kind.is_regular(), "{kind:?} 不该算普通文件");
        }
    }

    #[test]
    fn tail_takes_the_end_because_bsdtar_puts_the_verdict_there() {
        let stderr = "a: Can't create 'x': Invalid argument\nb: Can't create 'y': Invalid argument\ntar.exe: Error exit delayed from previous errors";
        assert!(tail(stderr, 1).contains("Error exit delayed"));
        assert!(tail(stderr, 2).contains("Can't create 'y'"));
        assert!(!tail(stderr, 1).contains("Can't create 'x'"));
        // 少于 n 行时全部返回，不 panic。
        assert_eq!(tail("only one", 20), "only one");
        assert_eq!(tail("", 20), "");
    }

    #[test]
    fn probe_finds_the_system_tar_and_prefers_system32() {
        // 这台机器上一定有（Windows 10 1803+ 自带）。没有的话这条测试
        // 会告诉我们"环境变了"，而不是静默跳过。
        let cli = TarCli::probe().expect("本机应当有 tar.exe");
        let program = cli.program().display().to_string().to_lowercase();
        assert!(program.ends_with("tar.exe"), "{program}");
        assert!(
            program.contains("system32"),
            "应当优先用 System32 里那份（PATH 上可能有 GNU tar）：{program}"
        );
    }
}
