//! `tuoen doctor` —— 环境体检。
//!
//! **只报告，不修改。** 这一票（票据 #13）的价值在于它是产品可信度的第一道门：
//! 把"`PATH` 上确认可执行"与"注册表声称已装但文件缺失"并列展示，用户看一眼就会
//! 决定信不信这个工具。
//!
//! # 形状：事实与判断分开
//!
//! ```text
//! collect_facts(ctx, opts) -> MachineFacts     ← 唯一碰机器的地方（只读）
//! diagnose(&MachineFacts)  -> Vec<Finding>     ← 纯函数，不 spawn、不读注册表
//! ```
//!
//! 这条分界不是为了好看，它买到三件事：
//!
//! 1. **检查项的测试不用造机器。** 每条检查的正例与反例都是一次结构体构造 ——
//!    而票据明确要求"每个正例都必须有对应的反例"，纯函数让那件事变得便宜。
//! 2. **`doctor` 与 `capture` 不可能漂移。** 事实就是 `capture` 的那四个文件形状
//!    （[`crate::capture::files`]），同一批采集器产出，于是"`capture` 写下的"
//!    与"`doctor` 诊断的"永远是同一件事。
//! 3. **检查项可以单独读。** 每个 `checks/*.rs` 是一个家族，每条检查旁边写着它的
//!    判据、严重度属于哪一类、以及它在真机上有没有实例。
//!
//! # 输出契约（票据给的五列，改动即为破坏性变更）
//!
//! | 列 | 含义 |
//! |---|---|
//! | `id` | 稳定机器可读 ID，见 [`ids`]。**未来要能按 ID 忽略/聚焦** |
//! | `severity` | `error` / `warn` / `info` |
//! | `message` | 中文人类可读（默认中文优先） |
//! | `evidence` | 具体是哪几条（路径、变量名、条目索引） |
//! | `source` | 这条结论从哪来（[`sources`]） |
//! | `confidence` | 与检测引擎同源的置信度（只有真的来自检测引擎时才有值） |
//!
//! `--json` 的输出必须稳定、**不本地化**（决策 35），所以 `message` 与 `evidence`
//! 里的中文只出现在人类输出里，机器读的是 `id` / `severity` / `source` / `confidence`。

pub mod checks;
pub mod facts;

pub use facts::{CommandResolution, DevRoot, GlobalPrefix, ShimFacts, SystemFacts};

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capture::files::{EnvFile, PathFile, ToolsFile, WslFile};
use crate::detect::DetectContext;

/// 全部稳定的 finding ID。**公开契约**：改了就是破坏性变更。
///
/// 票据把 23 个 ID 写死在表里，这里一个一个列出来（而不是让检查项自己拼字符串）：
/// 拼出来的 ID 会在某次重构里悄悄改名，而列表里的常量改不动 —— 有一条用例
/// 断言 [`ids::ALL`] 无重复、且每一个都被某个检查项真的用过。
pub mod ids {
    // ── path.* ──────────────────────────────────────────────────────
    pub const PATH_DUPLICATE: &str = "path.duplicate";
    pub const PATH_MISSING: &str = "path.missing";
    pub const PATH_USERNAME_HARDCODED: &str = "path.username-hardcoded";
    pub const PATH_SHADOWED: &str = "path.shadowed";
    pub const PATH_LENGTH_BUDGET: &str = "path.length-budget";
    pub const PATH_EMPTY_ENTRY: &str = "path.empty-entry";
    pub const PATH_REPARSE: &str = "path.reparse";
    pub const PATH_RELATIVE: &str = "path.relative";
    pub const PATH_NON_ASCII: &str = "path.non-ascii";
    pub const PATH_SPACES: &str = "path.spaces";

    // ── env.* ───────────────────────────────────────────────────────
    pub const ENV_MISSING_TARGET: &str = "env.missing-target";
    pub const ENV_DUPLICATED_SCOPE: &str = "env.duplicated-scope";
    pub const ENV_NAME_WITH_SPACES: &str = "env.name-with-spaces";
    pub const ENV_PATH_LITERAL: &str = "env.path-literal";

    // ── tool.* ──────────────────────────────────────────────────────
    pub const TOOL_MULTI_MANAGER: &str = "tool.multi-manager";
    pub const TOOL_MULTIPLE_ACTIVE: &str = "tool.multiple-active";
    pub const TOOL_GHOST: &str = "tool.ghost";
    pub const TOOL_GLOBAL_PREFIX_INSIDE_VERSION_DIR: &str = "tool.global-prefix-inside-version-dir";
    pub const TOOL_UNMANAGED_DIRECTORY: &str = "tool.unmanaged-directory";

    // ── system.* ────────────────────────────────────────────────────
    pub const SYSTEM_ELEVATED: &str = "system.elevated";
    pub const SYSTEM_DEVELOPER_MODE: &str = "system.developer-mode";
    pub const SYSTEM_LONG_PATHS: &str = "system.long-paths";
    pub const SYSTEM_WSL_NONSTANDARD_PATH: &str = "system.wsl-nonstandard-path";

    /// 23 个 ID，一个不多一个不少。用例钉住它与检查项的实际产出一致。
    pub const ALL: &[&str] = &[
        PATH_DUPLICATE,
        PATH_MISSING,
        PATH_USERNAME_HARDCODED,
        PATH_SHADOWED,
        PATH_LENGTH_BUDGET,
        PATH_EMPTY_ENTRY,
        PATH_REPARSE,
        PATH_RELATIVE,
        PATH_NON_ASCII,
        PATH_SPACES,
        ENV_MISSING_TARGET,
        ENV_DUPLICATED_SCOPE,
        ENV_NAME_WITH_SPACES,
        ENV_PATH_LITERAL,
        TOOL_MULTI_MANAGER,
        TOOL_MULTIPLE_ACTIVE,
        TOOL_GHOST,
        TOOL_GLOBAL_PREFIX_INSIDE_VERSION_DIR,
        TOOL_UNMANAGED_DIRECTORY,
        SYSTEM_ELEVATED,
        SYSTEM_DEVELOPER_MODE,
        SYSTEM_LONG_PATHS,
        SYSTEM_WSL_NONSTANDARD_PATH,
    ];
}

/// 一条结论的**来源**。`--json` 里是这些 slug 本身，不本地化。
pub mod sources {
    /// 注册表（卸载键、环境块、系统设置）。
    pub const REGISTRY: &str = "registry";
    /// `PATH` 解析（哪一条条目、第几条）。
    pub const PATH_RESOLUTION: &str = "path-resolution";
    /// 文件系统事实（存在、大小、reparse tag）。
    pub const FILESYSTEM: &str = "filesystem";
    /// 第三方版本管理器（nvm4w / uv / …）。
    pub const MANAGER: &str = "manager";
    /// tuoen 自己的安装记录。
    pub const TUOEN: &str = "tuoen";
    /// 操作系统事实（提权、Developer Mode、长路径开关）。
    pub const SYSTEM: &str = "system";
    /// WSL 的注册表子树。
    pub const WSL: &str = "wsl";
}

/// 严重度。**分配原则（票据给的）**：会导致**静默失效**的是 `error`；
/// 会导致**突然全体失效**的是 `error`；只是噪声的是 `info`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// 会静默失效，或者会突然全体失效。**要动手**。
    Error,
    /// 会咬人，但还没咬。
    Warn,
    /// 只是噪声，看一眼就好。
    Info,
}

impl Severity {
    /// `--json` 与人类输出共用的 slug。**不本地化。**
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
        }
    }

    /// 人类输出里那个词。
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "错误",
            Self::Warn => "警告",
            Self::Info => "提示",
        }
    }
}

/// 一条发现。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// 稳定 ID，取自 [`ids`]。
    pub id: &'static str,
    /// 严重度。
    pub severity: Severity,
    /// 中文一句话。**不进 `--json`**（决策 35：机器读 ID，不读中文）。
    #[serde(skip)]
    pub message: String,
    /// 具体是哪几条。人类输出用；`--json` 里也带（它是数据，不是散文）。
    ///
    /// **每一条都必须是数据**：`machine#21`、`C:\Program Files\…`、`user NVM_HOME`、
    /// `tool=java command=javac dir=C:\Dev\base\JDK\JDK8\bin` 这种"位置/名字 + 值"。
    /// **不许出现中文句子** —— `--json` 的成功载荷要能被脚本按字面匹配，中文解释在
    /// [`Finding::message`] 里（那一列 `#[serde(skip)]`，不进 JSON）。这条与
    /// `crates/cli/tests/path_contract.rs` 里"成功载荷无 CJK"那条断言同源。
    pub evidence: Vec<String>,
    /// 来源 slug，取自 [`sources`]。
    pub source: &'static str,
    /// 与检测引擎同源的置信度。
    ///
    /// **只有结论真的来自检测引擎的一行时才有值**（五个 `tool.*` 检查）。
    /// 其余检查的确定性由判据本身表达 —— 例如含 `%` 的 `PATH` 条目**根本不判**
    /// "失效"，所以 `path.missing` 报出来的每一条都是确定的。给它们编一个
    /// 置信度只会让那个字段变成装饰。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<&'static str>,
}

impl Finding {
    /// 造一条发现。`confidence` 默认没有，需要时用 [`Finding::with_confidence`]。
    #[must_use]
    pub fn new(
        id: &'static str,
        severity: Severity,
        message: impl Into<String>,
        evidence: Vec<String>,
        source: &'static str,
    ) -> Self {
        Self {
            id,
            severity,
            message: message.into(),
            evidence,
            source,
            confidence: None,
        }
    }

    #[must_use]
    pub fn with_confidence(mut self, confidence: &'static str) -> Self {
        self.confidence = Some(confidence);
        self
    }
}

/// `doctor` 的选项。
#[derive(Debug, Clone)]
pub struct DoctorOptions {
    /// 我们自己的 shim 目录。`None` 表示"不检查遮蔽"（没有 shim 就没有遮蔽可言）。
    pub shim_dir: Option<PathBuf>,
    /// tuoen 自己的根（store 与 shim 目录）—— `path.toml` 的 `owner` 判据要用。
    ///
    /// 类型与 `CaptureOptions::tuoen_roots` 一致（`PathBuf`），因为它直接传给同一个采集器。
    pub tuoen_roots: Vec<PathBuf>,
    /// 要不要跑"全局包前缀在哪"这类探测。**默认开**：它是 `error` 级检查的唯一输入。
    pub probe_prefixes: bool,
    /// 探测超时。一个挂住的工具不能挂住整个 `doctor`。
    pub probe_timeout: Duration,
}

impl DoctorOptions {
    /// 默认选项：不检查遮蔽、不探测前缀。
    #[must_use]
    pub fn new() -> Self {
        Self {
            shim_dir: None,
            tuoen_roots: Vec::new(),
            probe_prefixes: false,
            probe_timeout: Duration::from_secs(5),
        }
    }

    #[must_use]
    pub fn with_shim_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.shim_dir = Some(dir.into());
        self
    }

    #[must_use]
    pub fn with_tuoen_root(mut self, root: impl Into<PathBuf>) -> Self {
        self.tuoen_roots.push(root.into());
        self
    }

    #[must_use]
    pub fn probing_prefixes(mut self) -> Self {
        self.probe_prefixes = true;
        self
    }
}

impl Default for DoctorOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// 体检要用到的**全部事实**。
///
/// 前四个字段就是 `capture` 的四个文件形状 —— 同一批采集器产出，所以"写下的"
/// 与"诊断的"不可能漂移。后五个是本票新加的、`capture` 不需要的活事实。
#[derive(Debug, Clone)]
pub struct MachineFacts {
    /// `path.toml` 的形状。
    pub path: PathFile,
    /// `env.toml` 的形状。
    pub env: EnvFile,
    /// `tools.toml` 的形状。
    pub tools: ToolsFile,
    /// `wsl.toml` 的形状。
    pub wsl: WslFile,
    /// 操作系统事实（提权 / Developer Mode / 长路径）。
    pub system: SystemFacts,
    /// 每个已知工具的每条命令**解析到了哪个目录**（`tool.multiple-active` 的输入）。
    pub resolution: Vec<CommandResolution>,
    /// 第三方管理器的全局包前缀（`tool.global-prefix-inside-version-dir` 的输入）。
    pub global_prefix: Vec<GlobalPrefix>,
    /// 扫描根下面那些"看起来是开发工具、却没有任何机制提到它"的目录。
    pub dev_roots: Vec<DevRoot>,
    /// 我们自己的 shim：目录、盘上真的有哪些命令、那个目录在不在 `PATH` 上。
    pub shims: ShimFacts,
}

/// 体检结果。
#[derive(Debug, Clone)]
pub struct DoctorReport {
    /// 按（严重度，ID）排好序的发现。
    pub findings: Vec<Finding>,
    /// 各严重度的条数。
    pub counts: Counts,
    /// 一句话的数字摘要（人类输出与 `--json` 都用）。
    pub summary: FactsSummary,
}

/// 各严重度的条数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Counts {
    pub error: usize,
    pub warn: usize,
    pub info: usize,
}

impl Counts {
    /// 有没有 `error`。
    #[must_use]
    pub const fn has_error(&self) -> bool {
        self.error > 0
    }
}

/// 事实的规模。**分母必须说出来**：`path.shadowed` 报 0 条时，读的人要知道
/// 那是因为"没有 shim"还是"有 shim 但没被抢"。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsSummary {
    /// `PATH` 条目数（三个作用域）。
    pub path_entries: usize,
    /// 持久环境变量数（用户级 + 机器级）。
    pub env_vars: usize,
    /// 检测到的工具行数。
    pub tool_rows: usize,
    /// WSL 发行版数。
    pub wsl_distributions: usize,
    /// 盘上真实的 shim 数。
    pub shims_on_disk: usize,
    /// 我们解析过的命令数。
    pub resolved_commands: usize,
}

impl FactsSummary {
    /// 从事实里数出来。
    #[must_use]
    pub fn of(facts: &MachineFacts) -> Self {
        Self {
            path_entries: facts.path.entry.len(),
            env_vars: facts.env.var.len(),
            tool_rows: facts.tools.tool.len(),
            wsl_distributions: facts.wsl.distribution.len(),
            shims_on_disk: facts.shims.commands.len(),
            resolved_commands: facts.resolution.len(),
        }
    }
}

/// 跑一次体检：采集事实（只读），然后诊断。
///
/// 与 `capture` 一样，**一个字节都不写**。
#[must_use]
pub fn run(ctx: &DetectContext<'_>, opts: &DoctorOptions) -> DoctorReport {
    let facts = facts::collect_facts(ctx, opts);
    let findings = diagnose(&facts);
    let counts = count(&findings);
    let summary = FactsSummary::of(&facts);
    DoctorReport {
        findings,
        counts,
        summary,
    }
}

/// **纯函数**：从事实推出结论。不 spawn、不读注册表、不写任何东西。
///
/// 顺序是（严重度从重到轻，ID 字典序）—— 稳定的顺序让"两次 `doctor` 的输出
/// 逐字节相同"成立，也让 diff 有意义。
#[must_use]
pub fn diagnose(facts: &MachineFacts) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(checks::path::run(facts));
    findings.extend(checks::env::run(facts));
    findings.extend(checks::tool::run(facts));
    findings.extend(checks::system::run(facts));
    findings.sort_by(|a, b| (a.severity, a.id).cmp(&(b.severity, b.id)));
    findings
}

fn count(findings: &[Finding]) -> Counts {
    let mut counts = Counts::default();
    for finding in findings {
        match finding.severity {
            Severity::Error => counts.error += 1,
            Severity::Warn => counts.warn += 1,
            Severity::Info => counts.info += 1,
        }
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_id_table_has_no_duplicates_and_covers_all_four_families() {
        let mut sorted = ids::ALL.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            ids::ALL.len(),
            "ID 表里有重复：{:?}",
            ids::ALL
        );
        assert_eq!(ids::ALL.len(), 23, "票据的表里是 23 个 ID");
        for prefix in ["path.", "env.", "tool.", "system."] {
            assert!(
                ids::ALL.iter().any(|id| id.starts_with(prefix)),
                "少了 {prefix} 这一族"
            );
        }
    }

    #[test]
    fn severity_slugs_are_the_public_contract_and_are_not_localized() {
        assert_eq!(Severity::Error.as_str(), "error");
        assert_eq!(Severity::Warn.as_str(), "warn");
        assert_eq!(Severity::Info.as_str(), "info");
        assert!(Severity::Error < Severity::Warn, "排序：error 在最前");
        assert!(Severity::Warn < Severity::Info);
    }

    #[test]
    fn a_finding_serialises_without_its_chinese_message() {
        let finding = Finding::new(
            ids::PATH_EMPTY_ENTRY,
            Severity::Warn,
            "机器级 PATH 第 21 段是空的",
            vec!["machine#21".to_owned()],
            sources::REGISTRY,
        );
        let json = serde_json::to_string(&finding).expect("序列化");
        assert!(!json.contains("机器级"), "中文不许进 --json：{json}");
        assert!(json.contains("\"path.empty-entry\""));
        assert!(json.contains("\"warn\""));
        assert!(!json.contains("confidence"), "没有置信度时不留键：{json}");
    }
}
