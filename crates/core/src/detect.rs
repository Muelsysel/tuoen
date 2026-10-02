//! 检测引擎的**形状**（ticket #2 定型，ticket #11 填充实现）。
//!
//! 这一模块现在只定义类型，因为它编码的是**公开契约**：`--json` 输出里的取值、
//! 六个置信度层级、以及"每条记录都必须有来源"这条不变量。
//! 见 `docs/specs/L1-dev-state.md`。

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// 六层置信度。**层级数量与名字都是公开契约。**
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
        }
    }

    /// 这条记录是否代表"现在真的能用"。
    ///
    /// `RegisteredMissing` 与 `AliasGhost` 都是**看起来存在但用不了**的情形，
    /// 把它们与真实安装并列展示会让用户看一眼就不再信任这个工具。
    #[must_use]
    pub const fn is_usable(self) -> bool {
        matches!(self, Self::Managed | Self::Executable | Self::ManagerOwned)
    }
}

/// 检测结果汇总。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionSummary {
    pub tools: Vec<DetectedTool>,
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
