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
use tuoen_core::detect::{Confidence, DetectedTool, DetectionSummary};
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

// ─────────────────────────────────────────────────────────────────────────────
// `tuoen detect`
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen detect --json` 的载荷。
///
/// **六个来源与七层置信度的计数恒定出现**（即使是 0）。理由：消费者可以据此断言
/// "这一版引擎认这七层"，而不是从"某个层级缺席"里读出"这个层级被删了"。
/// 一个会随内容改变键集合的 JSON 不是稳定的接口。
#[derive(Debug, Serialize)]
pub struct DetectView {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    pub tool: Vec<DetectedToolView>,
    pub summary: DetectSummaryView,
}

/// 一条检测记录。
#[derive(Debug, Serialize)]
pub struct DetectedToolView {
    pub name: String,
    pub version: Option<String>,
    pub path: String,
    /// `tuoen` / `path-resolution` / `app-paths` / `registry-arp` /
    /// `filesystem-scan` / `manager` —— **不本地化**。
    pub source: &'static str,
    /// `managed` / `executable` / `manager-owned` / `directory-only` /
    /// `registered-missing` / `alias-ghost` —— **不本地化**。
    pub confidence: &'static str,
    /// 第三方版本管理器名（如 `nvm4w`），无则为 `null`。
    pub manager: Option<String>,
    /// 这一条是怎么被发现的。自由文本，**允许中文** —— 它是给人读的。
    pub evidence: String,
    /// 这一条能不能被 tuoen 在新机器上自动重建。
    ///
    /// 这是**派生的、但值得显式给出**的字段：消费者最常问的就是这个，
    /// 而让它自己从 `confidence` 推会把这套规则复制到每个消费者里。
    pub reproducible: bool,
}

#[derive(Debug, Serialize)]
pub struct DetectSummaryView {
    pub total: usize,
    #[serde(rename = "byConfidence")]
    pub by_confidence: Vec<CountView>,
    #[serde(rename = "bySource")]
    pub by_source: Vec<CountView>,
}

#[derive(Debug, Serialize)]
pub struct CountView {
    /// 层级或来源的稳定英文取值。
    pub key: &'static str,
    pub count: usize,
}

impl DetectView {
    #[must_use]
    pub fn new(summary: &DetectionSummary) -> Self {
        Self {
            schema_version: 1,
            tool: summary.tools.iter().map(DetectedToolView::from).collect(),
            summary: DetectSummaryView {
                total: summary.tools.len(),
                by_confidence: summary
                    .count_by_confidence()
                    .into_iter()
                    .map(|(level, count)| CountView {
                        key: level.as_str(),
                        count,
                    })
                    .collect(),
                by_source: summary
                    .count_by_source()
                    .into_iter()
                    .map(|(source, count)| CountView {
                        key: source.as_str(),
                        count,
                    })
                    .collect(),
            },
        }
    }
}

impl From<&DetectedTool> for DetectedToolView {
    fn from(tool: &DetectedTool) -> Self {
        Self {
            name: tool.name.clone(),
            version: tool.version.clone(),
            path: tool.path.clone(),
            source: tool.source.as_str(),
            confidence: tool.confidence.as_str(),
            manager: tool.manager.clone(),
            evidence: tool.evidence.clone(),
            reproducible: is_reproducible(tool.confidence),
        }
    }
}

/// 这一层置信度能不能被自动重建。
///
/// **判据本身在 [`Confidence::is_reproducible`] 上**（它是"捕获/还原"的核心规则，
/// 散落成多份 `match` 必然会漂移）；这个函数只是它的投影，供 `--json` 与人类输出共用。
#[must_use]
pub const fn is_reproducible(confidence: Confidence) -> bool {
    confidence.is_reproducible()
}

#[cfg(test)]
mod source_label_tests {
    use tuoen_core::detect::DetectionSource;

    #[test]
    fn source_values_are_the_ones_the_contract_names() {
        // 人类输出与 JSON 用**同一个**取值（`DetectionSource::as_str()`），
        // 否则"表格里写的名字"与"脚本里读到的名字"会漂移成两套词汇。
        //
        // 这个断言放在 CLI 侧而不是 core 侧，是因为**契约的这一头在这里**：
        // `detect_contract.rs` 断言 `--json` 里出现的就是这些字符串。
        assert_eq!(DetectionSource::AppPaths.as_str(), "app-paths");
        assert_eq!(DetectionSource::RegistryArp.as_str(), "registry-arp");
        assert_eq!(DetectionSource::PathResolution.as_str(), "path-resolution");
        assert_eq!(DetectionSource::FilesystemScan.as_str(), "filesystem-scan");
        assert_eq!(DetectionSource::Manager.as_str(), "manager");
        assert_eq!(DetectionSource::Tuoen.as_str(), "tuoen");
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

    #[test]
    fn detect_view_keeps_source_and_confidence_as_english_labels() {
        let summary = tuoen_core::detect::DetectionSummary {
            tools: vec![tuoen_core::detect::DetectedTool {
                name: "python".to_owned(),
                version: None,
                path: r"C:\Users\x\AppData\Local\Microsoft\WindowsApps\python.exe".to_owned(),
                source: tuoen_core::detect::DetectionSource::PathResolution,
                confidence: tuoen_core::detect::Confidence::AliasGhost,
                manager: None,
                evidence: "PATH 第 1 条里有 python.exe，但它是 0 字节的 App Execution Alias"
                    .to_owned(),
            }],
        };
        let view = DetectView::new(&summary);
        let json = serde_json::to_string(&view).expect("serialise");
        // 取值必须是英文小写 kebab —— 界面语言不能绑死脚本。
        assert!(json.contains(r#""source":"path-resolution""#), "{json}");
        assert!(json.contains(r#""confidence":"alias-ghost""#), "{json}");
        // 但 evidence 是给人读的，可以是中文。
        assert!(
            json.contains("幽灵") || json.contains("App Execution Alias"),
            "{json}"
        );
        // 顶层字段名。
        assert!(json.contains(r#""schemaVersion":1"#), "{json}");
    }

    #[test]
    fn detect_summary_lists_every_confidence_level_even_at_zero() {
        // 七个层级**恒定出现**：消费者可以据此断言"这一版引擎认这七层"，
        // 而不是从"某个层级缺席"里读出"这个层级不存在了"。
        let summary = tuoen_core::detect::DetectionSummary { tools: Vec::new() };
        let view = DetectView::new(&summary);
        let json = serde_json::to_string(&view).expect("serialise");
        for level in [
            "managed",
            "executable",
            "manager-owned",
            "directory-only",
            "registered-missing",
            "alias-ghost",
        ] {
            assert!(json.contains(level), "缺少层级 {level}：{json}");
        }
    }

    #[test]
    fn detect_view_always_reports_all_six_sources() {
        let summary = tuoen_core::detect::DetectionSummary { tools: Vec::new() };
        let json = serde_json::to_string(&DetectView::new(&summary)).expect("serialise");
        for source in [
            "tuoen",
            "path-resolution",
            "app-paths",
            "registry-arp",
            "filesystem-scan",
            "manager",
        ] {
            assert!(json.contains(source), "缺少来源 {source}：{json}");
        }
    }
}
