//! `tuoen catalog` 的 **`--json` 呈现层**。
//!
//! **为什么不直接序列化 `tuoen_manifest::Catalog`**：那是**磁盘格式**，
//! 它的字段名是 TOML 的形状（`schema_version` / `licence` / `recipe`），
//! 而且它会把 recipe 里**未渲染的 URL 模板**（含 `{version}`）暴露给消费者。
//!
//! `--json` 是**接口**：键用 camelCase，URL 是渲染后的真实地址，
//! 取值是稳定的机器可读字符串。两者混用会让磁盘格式变成事实上的公开 API ——
//! 以后想改 TOML 字段名就变成了破坏性变更。
//!
//! 见 `docs/specs/L1-dev-state.md` 的输出契约与 `docs/DESIGN.md` 决策 35。

use serde::Serialize;
use tuoen_manifest::{Catalog, GateVerdict, ResolvedRecipe, Tool, resolve};

/// `tuoen catalog list --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct CatalogListView {
    /// 这个 catalog 的名字（用户可能同时加载多个）。
    pub name: String,
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub tool: Vec<ToolSummary>,
}

/// 列表里的一个工具。**不含 recipe** —— 列表不该把每个版本的哈希都吐出来。
#[derive(Debug, Serialize)]
pub struct ToolSummary {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub homepage: Option<String>,
    pub licence: LicenceView,
    /// 可安装的版本数（不列内容）。
    #[serde(rename = "recipeCount")]
    pub recipe_count: usize,
}

#[derive(Debug, Serialize)]
pub struct LicenceView {
    /// `allowed` / `metadata-only` / `conditional` / `prohibited` —— **不本地化**。
    pub redistribution: &'static str,
    /// 许可证标识（SPDX 或名称）。
    pub name: String,
    /// 为什么是这个结论。自由文本，允许中文。
    pub notes: String,
    pub url: Option<String>,
}

impl From<&Tool> for ToolSummary {
    fn from(tool: &Tool) -> Self {
        Self {
            id: tool.id.clone(),
            display_name: tool.display_name.clone(),
            aliases: tool.aliases.clone(),
            description: tool.description.clone(),
            homepage: tool.homepage.clone(),
            licence: LicenceView {
                redistribution: tool.licence.redistribution.as_str(),
                name: tool.licence.spdx_or_name.clone(),
                notes: tool.licence.notes.clone(),
                url: tool.licence.url.clone(),
            },
            recipe_count: tool.recipes.len(),
        }
    }
}

impl From<&Catalog> for CatalogListView {
    fn from(catalog: &Catalog) -> Self {
        Self {
            name: catalog.name.clone(),
            schema_version: catalog.schema_version,
            tool: catalog.tools.iter().map(ToolSummary::from).collect(),
        }
    }
}

/// `tuoen catalog show <tool> --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct ToolDetailView {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub homepage: Option<String>,
    pub licence: LicenceView,
    pub versions: Vec<VersionView>,
}

/// 一个具体版本。**URL 是渲染后的真实地址，不是模板。**
#[derive(Debug, Serialize)]
pub struct VersionView {
    pub version: String,
    pub platform: String,
    /// 渲染后的下载地址。模板渲染失败时原样保留占位符 —— 那是一个可见的 bug 信号。
    pub url: String,
    pub archive: &'static str,
    pub sha256: String,
    /// 这个版本自己的许可证结论（已与工具层合并，**只紧不松**）。
    pub redistribution: &'static str,
    /// 这个版本能不能装。
    pub installable: bool,
}

impl ToolDetailView {
    /// 从工具 + 平台构造。每个版本的 URL 都走真正的渲染路径，
    /// 这样"模板写错了"会在 `catalog show` 里直接看见，而不是等到下载时才发现。
    #[must_use]
    pub fn new(catalog: &Catalog, tool: &Tool, platform: &str) -> Self {
        let versions = tool
            .recipes
            .iter()
            .filter(|r| r.platform == platform)
            .map(|r| {
                let resolved = resolve(catalog, &tool.id, &r.version, platform).ok();
                let (redistribution, url, sha256, archive, installable) = match resolved {
                    Some(recipe) => {
                        let verdict = tuoen_manifest::gate(&recipe);
                        (
                            recipe.redistribution.as_str(),
                            recipe.url,
                            recipe.checksum.value,
                            recipe.archive.as_str(),
                            verdict.is_allowed(),
                        )
                    }
                    // 解析失败：保留模板与原始哈希，并标成不可安装 —— 不要假装它可用。
                    None => (
                        r.licence
                            .as_ref()
                            .map_or(tool.licence.redistribution, |l| {
                                tool.licence
                                    .redistribution
                                    .more_restrictive(l.redistribution)
                            })
                            .as_str(),
                        r.url.clone(),
                        r.checksum.value.clone(),
                        r.archive.as_str(),
                        false,
                    ),
                };
                VersionView {
                    version: r.version.clone(),
                    platform: r.platform.clone(),
                    url,
                    archive,
                    sha256,
                    redistribution,
                    installable,
                }
            })
            .collect();

        Self {
            id: tool.id.clone(),
            display_name: tool.display_name.clone(),
            aliases: tool.aliases.clone(),
            description: tool.description.clone(),
            homepage: tool.homepage.clone(),
            licence: LicenceView {
                redistribution: tool.licence.redistribution.as_str(),
                name: tool.licence.spdx_or_name.clone(),
                notes: tool.licence.notes.clone(),
                url: tool.licence.url.clone(),
            },
            versions,
        }
    }
}

/// `tuoen catalog check <tool> <version> --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct CheckView {
    pub tool: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub version: String,
    pub platform: String,
    pub licence: LicenceView,
    pub installable: bool,
    /// 允许时是注意事项（如 NFTC 的"不收费"条件），拒绝时是**具体原因**。
    pub reason: String,
    /// 允许时才有：渲染后的下载地址。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archive: Option<&'static str>,
}

impl CheckView {
    #[must_use]
    pub fn from_resolved(recipe: &ResolvedRecipe, verdict: &GateVerdict) -> Self {
        let (installable, reason) = match verdict {
            GateVerdict::Allowed { notes } => (true, notes.clone()),
            GateVerdict::Rejected { reason } => (false, reason.clone()),
        };
        Self {
            tool: recipe.tool_id.clone(),
            display_name: recipe.display_name.clone(),
            version: recipe.version.clone(),
            platform: recipe.platform.clone(),
            licence: LicenceView {
                redistribution: recipe.redistribution.as_str(),
                name: recipe.licence_name.clone(),
                notes: recipe.licence_notes.clone(),
                url: None,
            },
            installable,
            reason,
            // 拒绝时**不给下载地址** —— 拒绝安装却顺手给出地址是自相矛盾的。
            url: installable.then(|| recipe.url.clone()),
            sha256: installable.then(|| recipe.checksum.value.clone()),
            archive: installable.then_some(recipe.archive.as_str()),
        }
    }

    /// 工具层就已经被判为不可再分发时的载荷（此时根本没有 recipe 可解析）。
    #[must_use]
    pub fn from_prohibited_tool(tool: &Tool, version: &str, platform: &str) -> Self {
        let verdict = tuoen_manifest::licence::evaluate(
            tool.licence.redistribution,
            &tool.licence.spdx_or_name,
            &tool.licence.notes,
        );
        let reason = match verdict {
            GateVerdict::Rejected { reason } => reason,
            GateVerdict::Allowed { notes } => notes,
        };
        Self {
            tool: tool.id.clone(),
            display_name: tool.display_name.clone(),
            version: version.to_owned(),
            platform: platform.to_owned(),
            licence: LicenceView {
                redistribution: tool.licence.redistribution.as_str(),
                name: tool.licence.spdx_or_name.clone(),
                notes: tool.licence.notes.clone(),
                url: tool.licence.url.clone(),
            },
            installable: false,
            reason,
            url: None,
            sha256: None,
            archive: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_manifest::PLATFORM_WINDOWS_X64;

    fn seed() -> Catalog {
        tuoen_manifest::load_seed().expect("seed")
    }

    #[test]
    fn list_view_uses_camel_case_keys_and_hides_recipes() {
        let catalog = seed();
        let view = CatalogListView::from(&catalog);
        let json = serde_json::to_string(&view).expect("serialise");
        assert!(json.contains(r#""schemaVersion""#), "{json}");
        assert!(json.contains(r#""displayName""#), "{json}");
        // 磁盘格式的字段名不得泄漏成接口。
        assert!(!json.contains(r#""schema_version""#), "{json}");
        assert!(!json.contains(r#""display_name""#), "{json}");
        // 列表不该吐每个版本的哈希。
        assert!(!json.contains("sha256"), "{json}");
        assert!(!json.contains("{version}"), "不得泄漏未渲染的模板：{json}");
    }

    #[test]
    fn detail_view_renders_urls_instead_of_leaking_templates() {
        let catalog = seed();
        let tool = tuoen_manifest::find_tool(&catalog, "temurin").expect("temurin");
        let view = ToolDetailView::new(&catalog, tool, PLATFORM_WINDOWS_X64);
        let json = serde_json::to_string(&view).expect("serialise");
        assert!(!json.contains("{version}"), "URL 必须已渲染：{json}");
        assert!(json.contains("jdk-21.0.12.1%2B1"), "{json}");
        assert!(json.contains(r#""installable":true"#), "{json}");
    }

    #[test]
    fn prohibited_tool_detail_has_no_installable_versions() {
        let catalog = seed();
        let tool = tuoen_manifest::find_tool(&catalog, "oracle-jdk").expect("oracle-jdk");
        let view = ToolDetailView::new(&catalog, tool, PLATFORM_WINDOWS_X64);
        assert!(view.versions.is_empty());
        assert_eq!(view.licence.redistribution, "prohibited");
    }

    #[test]
    fn check_view_omits_the_url_when_rejected() {
        let view = CheckView::from_prohibited_tool(
            tuoen_manifest::find_tool(&seed(), "oracle-jdk").expect("oracle-jdk"),
            "8",
            PLATFORM_WINDOWS_X64,
        );
        let json = serde_json::to_string(&view).expect("serialise");
        // 只断言"没有下载信息"。`licence.url` 是**许可证原文**地址，那个必须留着 ——
        // 用户需要它去读条款。
        assert!(!json.contains("\"sha256\""), "{json}");
        assert!(!json.contains("\"archive\""), "{json}");
        assert!(!json.contains("nodejs.org"), "{json}");
        // 不得给出**制品**地址。注意 `licence.url` 里的 `javase8-archive-downloads.html`
        // 是许可证/归档说明页，不是制品地址，所以这里断言的是制品路径的形状。
        assert!(!json.contains(".zip"), "{json}");
        assert!(!json.contains(".tar."), "{json}");
        assert!(json.contains(r#""installable":false"#), "{json}");
        assert!(json.contains("不可再分发"), "{json}");
    }

    #[test]
    fn check_view_includes_the_url_when_allowed() {
        let catalog = seed();
        let recipe = resolve(&catalog, "node", "24.19.0", PLATFORM_WINDOWS_X64).expect("resolve");
        let verdict = tuoen_manifest::gate(&recipe);
        let view = CheckView::from_resolved(&recipe, &verdict);
        let json = serde_json::to_string(&view).expect("serialise");
        assert!(json.contains("\"url\""), "{json}");
        assert!(json.contains("nodejs.org"), "{json}");
        assert!(json.contains(r#""installable":true"#), "{json}");
    }
}
