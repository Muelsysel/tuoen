//! `tuoen list` 的数据模型与实现。
//!
//! **这一票（ticket #6）把它从空壳变成真的。** ticket #2 只建立了形状：
//! 类型、序列化形状、输出契约（`--json` 稳定且不本地化）从第一天起就固定，
//! 后续票往里填数据而不是改形状 —— 这条约束现在兑现了：**唯一的改动是
//! 新增了一个 `installedVersions` 键**，原有的四个键一个没动。
//!
//! ## `list` 与 `detect` 的区别（这条区分是这个模块存在的理由）
//!
//! - `tuoen list` 读的是**我们自己的存储**：只有 tuoen 装的东西在这里。
//!   它的答案是"我管着哪些工具、现在生效哪个版本、还留着哪些版本"。
//! - `tuoen detect` 读的是**这台机器**：六个来源、七层置信度。
//!   它的答案是"这台机器上到底有什么，包括别人装的、只剩目录的、根本不存在的"。
//!
//! 把两者合成一个命令会让"我装了什么"和"这台机器上有什么"变成同一个问题 ——
//! 而它们恰恰是本机取证里最容易混淆的一对（`detect` 的七层里只有第一层
//! `managed` 与 `list` 说的是同一件事）。

use serde::{Deserialize, Serialize};
use tuoen_store::Store;

/// 一条工具记录在 `list` 输出里的形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRecord {
    /// 逻辑工具名（`node` / `python` / `java` …）。**小写，不含版本号。**
    pub name: String,
    /// 当前生效的版本。**没有任何版本被激活时是 `None`** —— 这与"没装过"不同，
    /// 后者根本不会出现在表里。
    pub version: Option<String>,
    /// 工具在存储里的目录（`…\store\node`）。**它在切版本时不变** ——
    /// 变的只是它里面的 `current` 指向哪。
    pub path: String,
    /// 这条记录是从哪来的。
    pub source: ToolSource,
    /// 存储里还留着的**全部**版本，**降序**（最新在最前）。
    ///
    /// 新增键（ticket #6）。消费者忽略未知键即可 —— 所以这是加法不是破坏。
    /// 为什么是"版本号的字符串数组"而不是带大小的对象：`list` 的用途是
    /// "我有什么、现在用哪个"，体积/装的时间属于 `tuoen list --long` 那一级，
    /// 现在还没有那个开关，所以先不把形状占住。
    ///
    /// **必须显式 rename。** 这个结构体没有 `rename_all = "camelCase"`
    /// （既有四个键都是单个单词，所以从来没暴露过这件事），于是漏了 rename
    /// 会让键变成 `installed_versions` —— 一个 snake_case 键混进一套 camelCase 契约里。
    /// 这条是**被 `manage_contract.rs` 的断言抓出来的**：它按契约写
    /// `data["tools"][0]["installedVersions"]`，拿到的是 `null`。
    #[serde(
        rename = "installedVersions",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    pub installed_versions: Vec<String>,
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
/// 列出 tuoen 自己管理的工具。
///
/// **接受一个 `&Store` 而不是自己去取默认位置** —— 这样测试能在临时目录上跑，
/// 而不会读到（并因此依赖）这台机器上真实的 `%LOCALAPPDATA%\tuoen\store`。
/// 一个"读机器真实状态"的函数是没法在 CI 上断言输出的。
///
/// 存储里**一个工具都没有**时返回空表，不是错误：刚装完 tuoen 的机器就是这样。
#[must_use]
pub fn list(store: &Store) -> ListResult {
    let tools = tuoen_store::installed_tools(store)
        .into_iter()
        .map(|tool| {
            let versions = tuoen_store::installed_versions(store, &tool);
            let active = tuoen_store::active_version(store, &tool);
            ToolRecord {
                path: store.tool_dir(&tool).display().to_string(),
                version: active,
                installed_versions: versions.into_iter().map(|v| v.version).collect(),
                name: tool,
                source: ToolSource::Tuoen,
            }
        })
        .collect();
    ListResult { tools }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::test_support::TempDir;

    /// 空的存储要产出**形状正确**的结果 —— 这是 ticket #2 钉下的契约，
    /// 现在由真实的（只是空的）存储来兑现，而不是一个恒返回空的函数。
    #[test]
    fn an_empty_store_lists_nothing() {
        let dir = TempDir::new("list-empty");
        let store = Store::new(dir.path());
        assert_eq!(list(&store), ListResult { tools: Vec::new() });
        let json = serde_json::to_string(&list(&store)).expect("serialise");
        assert_eq!(json, r#"{"tools":[]}"#);
    }

    #[test]
    fn a_tool_without_an_active_version_still_lists() {
        // "装了但没有生效版本"与"根本没装"必须能区分：
        // 前者 name 在、version 是 null；后者整行不存在。
        let dir = TempDir::new("list-no-active");
        let store = Store::new(dir.path());
        std::fs::create_dir_all(store.version_dir("node", "24.19.0")).expect("造一个版本目录");

        let result = list(&store);
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "node");
        assert_eq!(result.tools[0].version, None, "没有 current 就没有生效版本");
        assert_eq!(result.tools[0].source, ToolSource::Tuoen);
        assert_eq!(result.tools[0].installed_versions, vec!["24.19.0"]);
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
            installed_versions: Vec::new(),
        };
        let json = serde_json::to_string(&record).expect("serialise");
        assert!(json.contains(r#""source":"third-party-manager""#), "{json}");
        // 空的 installations 不该出现在输出里 —— 否则每个消费者都要处理一个空数组。
        assert!(
            !json.contains("installedVersions"),
            "空数组不该被序列化：{json}"
        );
        let back: ToolRecord = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, record);
    }

    #[test]
    fn a_consumer_that_does_not_know_installed_versions_still_parses() {
        // 这是"新增键是加法不是破坏"的**可执行证明**：
        // 老形状的 JSON（没有 installedVersions）必须还能读进来。
        let old = r#"{"name":"node","version":"24.19.0","path":"C:\\s\\node","source":"tuoen"}"#;
        let record: ToolRecord = serde_json::from_str(old).expect("老形状必须还能读");
        assert_eq!(record.installed_versions, Vec::<String>::new());
    }
}
