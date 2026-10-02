//! 检测引擎：把"这台机器上装了什么开发工具"读成结构化记录。
//!
//! **七层置信度**（[`Confidence`]）与**六种来源**（[`DetectionSource`]）是公开契约，
//! 它们编码的是本机取证里最贵的几条知识：
//!
//! - `alias-ghost` 必须独立存在：`WindowsApps\python.exe` 是 0 字节、
//!   reparse tag `0x8000001b` 的 App Execution Alias，而 `Get-Command python` **成功**
//!   并排在 `PATH` 最前。任何"`Test-Path` 通过就算存在"的实现都会在这里给出错误答案。
//! - `registered-missing` 必须与真实安装分开：本机 `Python311` 有 9 个活卸载键而
//!   `Test-Path` 为 `False`。把幽灵条目与真实安装并列展示，用户看一眼就不再信任这个工具。
//! - `manager-owned` 是**只读采纳**：能看见、能选中、能 pin，但 `uninstall` 不去动它们。
//!
//! **这一层只读。** 六个来源的实现见 [`engine`]。
//!
//! ## 测试约束（**这份文档曾经是假的，留下来当教训**）
//!
//! 检测引擎只依赖 [`context::DetectContext`] 里的 trait，所以**引擎自己的单测**
//! 全部注入固定装置，绝不读真机 —— `crates/core/src/detect/test_support.rs` 里
//! 的用例就是这样。
//!
//! **但 `crates/cli/tests/real_machine_acceptance.rs` 是有意读真机的**，
//! 而这一行早先写着"绝不读真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH`"——
//! 那是一句**不成立的宣称**：CLI 那一侧的契约测试跑的也是真的 `detect` 二进制
//! （它只断言形状与不变量，但**确实**读了这台机器）。
//!
//! 教训写在 `docs/acceptance/L1-01-detect.md`：**"测试不读真机"这句话在 trait 边界
//! 上成立，在进程边界上不成立。** 把两者混为一谈，得到的就是一句让人放心的假话 ——
//! 而一句假话比一个已知的取舍危险得多。
//!
//! 真实情况：**契约测试只断言形状**（干净 CI 上同样通过），
//! **真机验收测试有意读真机**，并在文件名与文件头注释里写明它是偏离。

pub mod context;
pub mod engine;
pub mod spec;
/// 测试支撑：把 `tuoen_platform::fixture` 的假机器接成 [`context::DetectContext`]。
///
/// **是公开 API 而不是 `#[cfg(test)]`**：`crates/cli/tests/*.rs` 是独立 crate，
/// 拿不到 `#[cfg(test)]` 的东西。
///
/// 注意它服务的对象（别把这句话读成"所有测试都不读真机"，见模块文档）：
/// 需要**固定装置**的用例走 `DetectFixture`，读真机的是另一个文件。
pub mod test_support;

pub use context::{DetectContext, PathEntry, PathScope, ScanRoot};
pub use spec::{KNOWN_TOOLS, ToolSpec, VersionStream};
pub use test_support::DetectFixture;

use serde::{Deserialize, Serialize};

/// 一条检测到的工具。
///
/// **不变量**：`source` 与 `confidence` 都必须有值 —— 不允许出现"没有来源"的条目，
/// 因为用户需要知道每条记录能不能在新机器上自动重建。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedTool {
    /// 逻辑工具名（`node` / `python` / `java` / `javac` …）。小写。
    pub name: String,
    /// 探测到的版本。发现但无法确定版本时为 `None`（例如命令超时）。
    pub version: Option<String>,
    /// 主路径（可执行文件或安装根目录）。
    pub path: String,
    /// 这条是怎么被发现的。
    pub source: DetectionSource,
    /// 这条有多可信。
    pub confidence: Confidence,
    /// 第三方版本管理器名（如 `nvm4w`），无则为 `None`。
    pub manager: Option<String>,
    /// 人类可读的一句话，说明这条是怎么被发现的。中文。
    pub evidence: String,
}

/// 检测来源。`--json` 里是这些字符串本身，**不本地化**。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DetectionSource {
    /// tuoen 自己的安装记录。
    Tuoen,
    /// `PATH` 解析。
    PathResolution,
    /// `App Paths` 注册表 —— 纯 `PATH` 扫描会整个漏掉这套查找机制。
    AppPaths,
    /// 注册表卸载键（ARP）。
    RegistryArp,
    /// 文件系统扫描。
    FilesystemScan,
    /// 第三方版本管理器。
    Manager,
}

impl DetectionSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tuoen => "tuoen",
            Self::PathResolution => "path-resolution",
            Self::AppPaths => "app-paths",
            Self::RegistryArp => "registry-arp",
            Self::FilesystemScan => "filesystem-scan",
            Self::Manager => "manager",
        }
    }
}

/// 七层置信度。**层级数量与名字都是公开契约。**
///
/// `AliasGhost` 必须独立存在：本机实测 `WindowsApps\python.exe` 是 0 字节、
/// reparse tag `0x8000001b` 的 App Execution Alias，而 `Get-Command python` **成功**并排在
/// `PATH` 最前。任何"`Test-Path` 通过就算存在"的实现都会在这里给出错误答案。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Confidence {
    /// 由 tuoen 自己安装。
    Managed,
    /// `PATH` 上能解析到，目标不是 alias，文件真实存在且大小 > 0。
    Executable,
    /// 注册表声称已安装，但文件不存在 —— **幽灵条目**。
    RegisteredMissing,
    /// 发现目录，未在任何注册表 / `PATH` / `App Paths` 里注册。
    DirectoryOnly,
    /// App Execution Alias（reparse tag `0x8000001b`、长度 0）：`Test-Path` 通过但不是文件。
    AliasGhost,
    /// 由第三方版本管理器管理（只读采纳）。
    ManagerOwned,
    /// 注册表声称已安装，且**安装目录真的存在**，但它不在 `PATH` 上。
    ///
    /// **这一层是独立验收逼出来的**：ARP 的 `InstallLocation` 常常是一个**目录**
    /// （本机 `C:\Program Files\Git\` / `C:\Program Files\Java\jre1.8.0_491\` /
    /// `C:\Program Files\WSL Dashboard\`）。把它们报成 `executable` 是错的 ——
    /// `executable` 的判据是"在 `PATH` 上能解析到、文件真实存在且大小 > 0"，
    /// 而这三条一条都不满足。
    ///
    /// 它与 `DirectoryOnly` 的区别是**有注册表记录**（因此能被自动重建），
    /// 与 `RegisteredMissing` 的区别是**目录真的在**（所以不是幽灵）。
    Registered,
}

impl Confidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Executable => "executable",
            Self::RegisteredMissing => "registered-missing",
            Self::DirectoryOnly => "directory-only",
            Self::AliasGhost => "alias-ghost",
            Self::ManagerOwned => "manager-owned",
            Self::Registered => "registered",
        }
    }

    /// 这一层能不能被 tuoen 在新机器上自动重建。
    ///
    /// **这个判断放在 `Confidence` 上而不是 CLI 里**：它是"捕获/还原"的核心判据，
    /// 散落成多份 `match` 必然会漂移。`--json` 的 `reproducible` 字段就是它的投影。
    ///
    /// - `managed` / `executable` / `registered` → 能（我们知道它是什么、在哪）
    /// - `manager-owned` → **不能由我们重建**（那是别人的版本管理器，我们只读采纳）
    /// - `directory-only` → 不能（知道目录在哪，但不知道它怎么装上去的）
    /// - `registered-missing` → 不能（它根本不存在）
    /// - `alias-ghost` → 不能（那是系统别名，不是安装）
    #[must_use]
    pub const fn is_reproducible(self) -> bool {
        matches!(self, Self::Managed | Self::Executable | Self::Registered)
    }

    /// 这条记录是否代表"现在真的能用"。
    ///
    /// `RegisteredMissing` 与 `AliasGhost` 都是**看起来存在但用不了**的情形，
    /// 把它们与真实安装并列展示会让用户看一眼就不再信任这个工具。
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(
            self,
            Self::Managed | Self::Executable | Self::ManagerOwned | Self::Registered
        )
    }
}

/// 检测结果汇总。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionSummary {
    pub tools: Vec<DetectedTool>,
}

impl DetectionSummary {
    /// 按置信度层级分组计数（给人类输出用）。
    #[must_use]
    pub fn count_by_confidence(&self) -> Vec<(Confidence, usize)> {
        // 固定顺序，便于稳定输出。
        const ORDER: &[Confidence] = &[
            Confidence::Managed,
            Confidence::Executable,
            Confidence::Registered,
            Confidence::ManagerOwned,
            Confidence::DirectoryOnly,
            Confidence::RegisteredMissing,
            Confidence::AliasGhost,
        ];
        ORDER
            .iter()
            .map(|level| {
                (
                    *level,
                    self.tools.iter().filter(|t| t.confidence == *level).count(),
                )
            })
            .collect()
    }

    /// 按来源分组计数。
    #[must_use]
    pub fn count_by_source(&self) -> Vec<(DetectionSource, usize)> {
        engine::SOURCE_ORDER
            .iter()
            .map(|source| {
                (
                    *source,
                    self.tools.iter().filter(|t| t.source == *source).count(),
                )
            })
            .collect()
    }
}

/// **检测引擎的入口**：跑六个来源、合并、返回汇总。
///
/// 六个来源的**执行顺序不影响结果**（合并时按 [`engine::SOURCE_ORDER`] 重排），
/// 但它们的**代价差别很大**：`PATH` 解析要对每个条目做定向探测，
/// 文件系统扫描要列目录，而版本探测要起进程。所以 `probe_versions = false`
/// 时整个检测只做结构查询，`doctor` 用它。
#[must_use]
pub fn detect_all(ctx: &DetectContext<'_>) -> DetectionSummary {
    let entries = engine::path_entries(ctx);

    let groups = vec![
        engine::from_managed(ctx),
        engine::from_path(ctx, &entries),
        engine::from_app_paths(ctx),
        engine::from_arp(ctx),
        engine::from_filesystem_scan(ctx),
        engine::from_managers(ctx),
    ];

    DetectionSummary {
        tools: engine::merge(groups),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_labels_are_the_public_contract() {
        assert_eq!(Confidence::Managed.as_str(), "managed");
        assert_eq!(Confidence::Executable.as_str(), "executable");
        assert_eq!(Confidence::RegisteredMissing.as_str(), "registered-missing");
        assert_eq!(Confidence::DirectoryOnly.as_str(), "directory-only");
        assert_eq!(Confidence::AliasGhost.as_str(), "alias-ghost");
        assert_eq!(Confidence::ManagerOwned.as_str(), "manager-owned");
    }

    #[test]
    fn source_labels_are_the_public_contract() {
        assert_eq!(DetectionSource::Tuoen.as_str(), "tuoen");
        assert_eq!(DetectionSource::PathResolution.as_str(), "path-resolution");
        assert_eq!(DetectionSource::AppPaths.as_str(), "app-paths");
        assert_eq!(DetectionSource::RegistryArp.as_str(), "registry-arp");
        assert_eq!(DetectionSource::FilesystemScan.as_str(), "filesystem-scan");
        assert_eq!(DetectionSource::Manager.as_str(), "manager");
    }

    #[test]
    fn aliases_and_ghosts_are_not_usable() {
        // 这是本机取证里最贵的一课：Test-Path 通过 ≠ 能用。
        assert!(!Confidence::AliasGhost.is_usable());
        assert!(!Confidence::RegisteredMissing.is_usable());
        assert!(!Confidence::DirectoryOnly.is_usable());
        assert!(Confidence::Executable.is_usable());
        assert!(Confidence::Managed.is_usable());
        assert!(Confidence::ManagerOwned.is_usable());
    }

    #[test]
    fn json_uses_kebab_case_enum_values() {
        let tool = DetectedTool {
            name: "python".to_owned(),
            version: None,
            path: r"C:\Users\example\AppData\Local\Microsoft\WindowsApps\python.exe".to_owned(),
            source: DetectionSource::PathResolution,
            confidence: Confidence::AliasGhost,
            manager: None,
            evidence: "PATH 上排在最前，但是 0 字节的 App Execution Alias".to_owned(),
        };
        let json = serde_json::to_string(&tool).expect("serialise");
        assert!(json.contains(r#""confidence":"alias-ghost""#), "{json}");
        assert!(json.contains(r#""source":"path-resolution""#), "{json}");
    }
}
