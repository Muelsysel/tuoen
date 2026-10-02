//! 下载层的错误。
//!
//! **这一层的错误分类是产品功能，不是内部细节。**
//!
//! 票据 #4 要求"下载失败时区分并报告原因：网络不可达 / 镜像失败 / 哈希不符"。
//! 这三者对用户的**下一步动作完全不同**：
//!
//! | 分类 | 用户该做什么 |
//! |---|---|
//! | 网络不可达 | 检查代理/防火墙，或换网络 |
//! | 镜像失败 | 换源（我们会自动做），或自定义镜像模板 |
//! | 哈希不符 | **停下来** —— 要么上游换了制品，要么有人在中间改包 |
//!
//! 所以 [`DownloadError`] 的每个变体都带**够用户自己核对的原始值**
//! （期望哈希 vs 实际哈希、HTTP 码、curl 退出码）。只说"下载失败"等于把
//! 用户丢在一个没有出口的错误前面。

use std::path::PathBuf;

use tuoen_platform::PlatformError;

/// 一次下载尝试失败的原因。
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    /// 传输失败（进程没起来、超时、非零退出码）。
    ///
    /// `code` 是 `curl` 的退出码 —— **这是我们能给用户的最有诊断价值的东西**：
    /// `6` 是 DNS 解析失败、`7` 是连不上、`28` 是超时、`35` 是 TLS 握手失败、
    /// `56` 是接收数据失败。把它丢掉会让"网络不可达"变成一个不可区分的大类。
    #[error("取 {url} 失败（curl 退出码 {code}）：{stderr}")]
    Transport {
        /// 出问题的 URL。
        url: String,
        /// `curl` 的退出码。
        code: i32,
        /// 被截断的 stderr。
        stderr: String,
    },

    /// 服务器返回了非 2xx。
    ///
    /// **与 [`Self::Transport`] 分开**：HTTP 404/403 说明"这个源上没有这个文件"
    /// （换源有意义），而连不上说明"这个源根本到不了"（换源也有意义，但原因不同）。
    #[error("{url} 返回 HTTP {status}")]
    Http {
        /// 出问题的 URL。
        url: String,
        /// HTTP 状态码。
        status: u16,
    },

    /// 下载回来的字节与登记的 SHA256 不符。
    ///
    /// **这个变体必须两个值都给全。** 只说"校验失败"会让用户无法判断
    /// "是我本地的环境问题"还是"上游真的换了包"。
    #[error(
        "SHA256 不符：期望 {expected}，实际 {actual}（{url}，{bytes} 字节）。\
         这个制品可能已经在上游被替换，也可能有人在中间改过它 —— 两种都不应该继续安装"
    )]
    ChecksumMismatch {
        /// 登记的哈希。
        expected: String,
        /// 实际算出来的哈希。
        actual: String,
        /// 从哪来的。
        url: String,
        /// 多少字节。
        bytes: u64,
    },

    /// 登记的哈希算法我们不认识。
    ///
    /// **不认识就不算**：把 `sha512` 当成"某种哈希然后放过去"是安全漏洞。
    #[error("不支持的哈希算法 `{algorithm}`（目前只支持 sha256）")]
    UnsupportedAlgorithm {
        /// 制品里登记的算法名。
        algorithm: String,
    },

    /// 本地归档文件不存在或不是文件。
    #[error("本地归档文件不可用：{path}")]
    LocalFileUnavailable {
        /// 那个路径。
        path: PathBuf,
    },

    /// 所有候选源都失败了。
    ///
    /// **保留每一次尝试**：用户需要看到"阿里 403、清华 404、官方超时"这种逐条结果，
    /// 而不是一个"都失败了"。
    #[error("全部 {attempts} 个候选源都失败：{}", .failures.iter().map(|f| format!("{f}")).collect::<Vec<_>>().join("；"))]
    AllSourcesFailed {
        /// 试了几个源。
        attempts: usize,
        /// 每一个的失败原因，**按尝试顺序**。
        failures: Vec<DownloadError>,
    },

    /// 缓存目录不可用（建不出来、或写入失败）。
    ///
    /// **缓存失败不该让下载失败** —— 见 [`crate::cache`]。这个变体只用于
    /// "缓存目录本身就无法使用"（例如 `%APPDATA%` 不可写），那时我们会
    /// **降级到不缓存**，而不是让用户下载不了。
    #[error("缓存目录不可用（{path}）：{source}")]
    CacheUnavailable {
        /// 缓存目录。
        path: PathBuf,
        /// 底层 IO 错误。
        #[source]
        source: std::io::Error,
    },

    /// 平台层错误（读注册表/文件系统求 `%APPDATA%` 时）。
    #[error(transparent)]
    Platform(#[from] PlatformError),
}

impl DownloadError {
    /// 这一条失败属于哪一类。给"回退策略"与"人类可读的分类报告"用。
    #[must_use]
    pub const fn kind(&self) -> FailureKind {
        match self {
            // 传输失败与 HTTP 失败都是"这个源这次不行" → 换源。
            Self::Transport { .. } | Self::Http { .. } => FailureKind::SourceUnavailable,
            // **哈希不符不是"换个源再试"** —— 它是"停下来"。
            Self::ChecksumMismatch { .. } => FailureKind::Integrity,
            Self::UnsupportedAlgorithm { .. } => FailureKind::Integrity,
            Self::LocalFileUnavailable { .. } => FailureKind::LocalInput,
            Self::AllSourcesFailed { .. } => FailureKind::SourceUnavailable,
            Self::CacheUnavailable { .. } => FailureKind::LocalEnvironment,
            Self::Platform(_) => FailureKind::LocalEnvironment,
        }
    }

    /// **这个失败该不该触发回退到下一个源。**
    ///
    /// 这是一条安全判断，不是便利判断：哈希不符时**回退到另一个镜像去拿"另一份"
    /// 同样的制品**有可能拿到一份哈希对得上的（因为攻击者只污染了其中一个镜像），
    /// 但那是在**掩盖一次完整性事件**。正确行为是停下来、把它报出来、让人看。
    #[must_use]
    pub const fn should_try_next_source(&self) -> bool {
        matches!(self.kind(), FailureKind::SourceUnavailable)
    }
}

/// 失败的类别。**取值是公开契约**（`--json` 里会出现），所以不本地化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FailureKind {
    /// 这个源这次到不了（网络、超时、HTTP 错误码）。换源有意义。
    SourceUnavailable,
    /// **完整性出了问题**（哈希不符、不认识的算法）。换源**没有**意义。
    Integrity,
    /// 用户给的本地输入有问题（文件不存在）。
    LocalInput,
    /// 本机环境的问题（缓存目录、`%APPDATA%`）。
    LocalEnvironment,
}

impl FailureKind {
    /// `--json` 与固定装置里的稳定取值，**不本地化**。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "source-unavailable",
            Self::Integrity => "integrity",
            Self::LocalInput => "local-input",
            Self::LocalEnvironment => "local-environment",
        }
    }

    /// 给用户看的建议。**这是人类输出，可以是中文。**
    #[must_use]
    pub const fn advice(self) -> &'static str {
        match self {
            Self::SourceUnavailable => {
                "换一个源，或者用 `--mirror` 指定你自己的镜像模板；也可以先跑 `tuoen fetch --probe-sources` 看哪个源在这个网络上真的可用"
            }
            Self::Integrity => {
                "停下来核对：上游可能替换了这个制品，也可能有人在中间改过它。不要重试到通过为止"
            }
            Self::LocalInput => "检查你给的路径",
            Self::LocalEnvironment => {
                "检查 %APPDATA% 是否可写；缓存不可用不会阻止下载，但环境本身有问题会"
            }
        }
    }
}
