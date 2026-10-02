//! `tuoen list` 的数据模型与实现。
//!
//! 这一票（ticket #2）只建立**形状**：`list` 目前返回空列表，但类型、序列化形状、
//! 输出契约（`--json` 稳定且不本地化）从第一天起就固定下来，后续票往里填数据而不是改形状。

use serde::{Deserialize, Serialize};

/// 一条工具记录在 `list` 输出里的形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRecord {
    /// 逻辑工具名（`node` / `python` / `java` …）。**小写，不含版本号。**
    pub name: String,
    /// 当前生效的版本。发现但无法确定版本时为 `None`。
    pub version: Option<String>,
    /// 主路径（可执行文件或安装根目录）。
    pub path: String,
    /// 这条记录是从哪来的。
    pub source: ToolSource,
}

/// 工具的来源。**`--json` 输出里的取值是这些字符串本身，不本地化。**
///
/// 增删变体是破坏性变更：见 `docs/specs/L1-dev-state.md` 的输出契约一节。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolSource {
    /// 由 tuoen 自己安装。
    Tuoen,
    /// 由第三方版本管理器管理（只读采纳）。
    ThirdPartyManager,
    /// 系统包管理器（winget / scoop）。
    SystemManager,
    /// 注册表卸载键。
    Registry,
    /// 仅在文件系统上发现目录。
    Filesystem,
    /// 通过 `PATH` 解析发现。
    PathResolution,
}

impl ToolSource {
    /// 供人类输出使用的稳定标签。**英文，不本地化**（`--json` 与人类输出共用同一取值）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tuoen => "tuoen",
            Self::ThirdPartyManager => "third-party-manager",
            Self::SystemManager => "system-manager",
            Self::Registry => "registry",
            Self::Filesystem => "filesystem",
            Self::PathResolution => "path-resolution",
        }
    }
}

/// `list` 的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListResult {
    pub tools: Vec<ToolRecord>,
}

/// 列出已知工具。
///
/// **这一票返回空列表** —— 检测引擎在 ticket #11 落地后由 `tuoen-core::detect` 提供数据。
/// 现在返回空不是"占位实现"，而是这一票的验收要求：`tuoen list` 必须能端到端跑通并输出
/// 一个**形状正确**的空结果。
#[must_use]
pub fn list() -> ListResult {
    ListResult { tools: Vec::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_list_serialises_to_stable_shape() {
        let json = serde_json::to_string(&list()).expect("serialise");
        assert_eq!(json, r#"{"tools":[]}"#);
    }

    #[test]
    fn source_labels_are_kebab_case_and_not_localised() {
        // 这些字符串是公开契约：改动它们会破坏脚本与未来的 GUI。
        assert_eq!(ToolSource::Tuoen.as_str(), "tuoen");
        assert_eq!(
            ToolSource::ThirdPartyManager.as_str(),
            "third-party-manager"
        );
        assert_eq!(ToolSource::SystemManager.as_str(), "system-manager");
        assert_eq!(ToolSource::Registry.as_str(), "registry");
        assert_eq!(ToolSource::Filesystem.as_str(), "filesystem");
        assert_eq!(ToolSource::PathResolution.as_str(), "path-resolution");
    }

    #[test]
    fn record_round_trips_through_json() {
        let record = ToolRecord {
            name: "node".to_owned(),
            version: Some("24.19.0".to_owned()),
            path: r"C:\Users\example\AppData\Local\nvm\v24.19.0".to_owned(),
            source: ToolSource::ThirdPartyManager,
        };
        let json = serde_json::to_string(&record).expect("serialise");
        assert!(json.contains(r#""source":"third-party-manager""#), "{json}");
        let back: ToolRecord = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, record);
    }
}
