//! manifest 的两层模型。
//!
//! - **catalog**（[`Catalog`]）—— 工具元数据："这是什么工具"、**它的许可证是什么**
//! - **recipe**（[`Recipe`]）—— 某个具体版本的安装步骤："这个版本怎么装"
//!
//! **为什么必须两层**（`docs/DESIGN.md` 决策 18）：许可证字段必须挂在"工具"层，
//! 但 Oracle 的规则**按版本分界**（JDK 21+ 为 NFTC 可镜像；JDK 8/11/17 不可再分发），
//! 两层的切分点正好落在"版本"上。
//!
//! **本模块只描述形状，不做校验** —— 校验在 [`crate::validate`]，
//! 这样畸形输入能一次报出全部问题而不是撞到第一个就停。

use serde::{Deserialize, Serialize};

pub use crate::licence::{ArchiveFormat, Redistribution};

/// 整个 catalog 文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    /// **格式版本。** 未来改 schema 时用它区分，而不是靠猜。
    pub schema_version: u32,
    /// 这个文件的名字，出现在错误信息里（用户可能同时加载多个）。
    #[serde(default)]
    pub name: String,
    /// 工具列表。
    #[serde(default, rename = "tool")]
    pub tools: Vec<Tool>,
}

/// 一个工具。**注意这一层没有版本号** —— 版本属于 recipe。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tool {
    /// 稳定 id，小写，无空格（`node` / `temurin` / `oracle-jdk`）。
    pub id: String,
    /// 人类可读的显示名。
    pub display_name: String,
    /// 别名，用于 `tuoen install <alias>`。
    #[serde(default)]
    pub aliases: Vec<String>,
    /// 描述。中文优先。
    #[serde(default)]
    pub description: String,
    /// 上游项目地址。
    #[serde(default)]
    pub homepage: Option<String>,
    /// **工具的许可证结论。这是模型里的一等字段，不是法务脚注。**
    pub licence: ToolLicence,
    /// 可用的版本来源。决定"能装哪些版本"。
    #[serde(default, rename = "version_source")]
    pub version_sources: Vec<VersionSource>,
    /// 具体版本的安装步骤。
    #[serde(default, rename = "recipe")]
    pub recipes: Vec<Recipe>,
}

/// 工具层的许可证信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLicence {
    /// 再分发结论。
    pub redistribution: Redistribution,
    /// 许可证标识（`MIT` / `GPL-2.0-with-classpath-exception` / `NFTC` …）。
    pub spdx_or_name: String,
    /// 为什么是这个结论。**必须具体** —— "许可问题"对用户毫无用处。
    #[serde(default)]
    pub notes: String,
    /// 许可证原文地址。
    #[serde(default)]
    pub url: Option<String>,
}

/// 版本来源。
///
/// 两种形态对应两种现实：有的上游有机器可读的版本索引，有的只能靠"构造 URL 试"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum VersionSource {
    /// 上游有 JSON 版本索引。`index_url` 是那个索引的地址。
    Api {
        index_url: String,
        /// 从索引里取版本号用的 JSON 指针或字段名（如 `version`）。
        #[serde(default)]
        version_field: Option<String>,
    },
    /// 没有索引：靠 URL 模板构造，能用 HTTP 探测确认存在。
    Template { url_template: String },
    /// 需要 `tuoen` 自己的远程索引（决策 19：内置种子 + 可选远程签名索引）。
    RemoteIndex { index_url: String },
}

/// 某个具体版本的安装步骤。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    /// 这个 recipe 覆盖的版本（精确版本或前缀约束，如 `24` / `24.19.0`）。
    pub version: String,
    /// 目标平台三元组（`windows-x64` / `windows-arm64`）。
    pub platform: String,
    /// 下载地址模板。
    pub url: String,
    /// 校验。**SHA256 是必需的** —— 没有哈希就没有供应链保证。
    pub checksum: Checksum,
    /// 归档格式。决定走哪条解压路径。
    pub archive: ArchiveFormat,
    /// 解压后的布局与暴露方式。
    pub layout: Layout,
    /// **可选**的版本级许可证覆盖。
    ///
    /// 存在理由：Oracle JDK 21+ 是 NFTC（有条件可再分发），而 8/11/17 不可再分发。
    /// **覆盖只能更严格，不能放宽** —— 见 [`crate::licence`]。
    #[serde(default)]
    pub licence: Option<ToolLicence>,
}

/// 校验信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    /// 算法。目前只支持 `sha256`。
    pub algorithm: String,
    /// 十六进制摘要。小写比较。
    pub value: String,
}

/// 解压后的布局。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Layout {
    /// 剥掉顶层目录的层数。上游归档普遍套一层 `<tool>-<version>-<platform>/`。
    #[serde(default)]
    pub strip_components: u32,
    /// 要暴露的命令：命令名 → 归档内的相对路径。
    ///
    /// **注意**：这里只描述"有哪些命令"，**不决定怎么暴露**。暴露方式是 shim
    /// （见 `docs/adr/0002`），而 `.cmd` / `.ps1` 也在列表里是合法的 ——
    /// 它们是**被 shim 转发的目标**，不是发给用户的 shim。
    #[serde(default)]
    pub bin: std::collections::BTreeMap<String, String>,
    /// 归档内被视为"环境根"的相对路径（如 JDK 的 `.`）。
    #[serde(default)]
    pub home: Option<String>,
}

/// 一个已解析出来的、可安装的具体版本。
///
/// 由 [`crate::resolve`] 从 [`Tool`] + [`Recipe`] 组合而成 ——
/// **许可证结论也在这时定下来**（工具层与 recipe 层取更严格的那个）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedRecipe {
    pub tool_id: String,
    pub display_name: String,
    pub version: String,
    pub platform: String,
    pub url: String,
    pub checksum: Checksum,
    pub archive: ArchiveFormat,
    pub layout: Layout,
    /// **生效的**再分发结论（工具层与 recipe 层里更严格的那个）。
    pub redistribution: Redistribution,
    /// 许可证标识。
    pub licence_name: String,
    /// 为什么是这个结论。
    pub licence_notes: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_round_trips_through_toml() {
        let text = r#"
schema_version = 1
name = "seed"

[[tool]]
id = "node"
display_name = "Node.js"
licence = { redistribution = "allowed", spdx_or_name = "MIT" }

  [[tool.recipe]]
  version = "24.19.0"
  platform = "windows-x64"
  url = "https://example.invalid/node-{version}.zip"
  archive = "zip"
  checksum = { algorithm = "sha256", value = "aa" }
  layout = { strip_components = 1, bin = { node = "node.exe" } }
"#;
        let catalog: Catalog = toml::from_str(text).expect("parse");
        assert_eq!(catalog.tools.len(), 1);
        assert_eq!(catalog.tools[0].recipes.len(), 1);
        assert_eq!(catalog.tools[0].recipes[0].layout.strip_components, 1);
    }

    #[test]
    fn version_source_is_tagged_by_kind() {
        let text = r#"
schema_version = 1
[[tool]]
id = "node"
display_name = "Node.js"
licence = { redistribution = "allowed", spdx_or_name = "MIT" }
version_source = [{ kind = "template", url_template = "https://x/{version}/" }]
"#;
        let catalog: Catalog = toml::from_str(text).expect("parse");
        assert!(matches!(
            catalog.tools[0].version_sources[0],
            VersionSource::Template { .. }
        ));
    }

    #[test]
    fn recipe_without_licence_has_none() {
        let recipe = Recipe {
            version: "1".to_owned(),
            platform: "windows-x64".to_owned(),
            url: "https://x".to_owned(),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "aa".to_owned(),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout::default(),
            licence: None,
        };
        assert!(recipe.licence.is_none());
    }
}
