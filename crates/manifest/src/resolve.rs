//! 版本解析：把 catalog 里的 [`Tool`] + [`Recipe`] 组合成一个可安装的具体版本，
//! **并在这里定下生效的许可证结论**。
//!
//! **"覆盖只能更严格"这条不变量在这里落地**（见 [`crate::licence`]）：
//! 一个 recipe 带着 `allowed` 不能洗白工具层的 `prohibited`。
//!
//! 这一层**不下载任何东西** —— 它只回答"能不能装、装哪个、从哪装、哈希是什么"。

use crate::licence::Redistribution;
use crate::model::{Catalog, Layout, ResolvedRecipe, Tool};

/// 解析失败的原因。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolveError {
    #[error("catalog 里没有工具 `{0}`（别名也算）")]
    UnknownTool(String),

    #[error("工具 `{tool}` 没有适用于平台 `{platform}` 的 recipe")]
    NoPlatform { tool: String, platform: String },

    #[error("工具 `{tool}` 没有匹配版本约束 `{constraint}` 的 recipe；已知版本：{known}")]
    NoVersion {
        tool: String,
        constraint: String,
        known: String,
    },
}

/// 平台三元组。**V1 只有 Windows**，但类型上不硬编码，便于将来加。
pub const PLATFORM_WINDOWS_X64: &str = "windows-x64";
pub const PLATFORM_WINDOWS_ARM64: &str = "windows-arm64";

/// 按 id **或别名**找一个工具。大小写不敏感。
#[must_use]
pub fn find_tool<'a>(catalog: &'a Catalog, id_or_alias: &str) -> Option<&'a Tool> {
    let needle = id_or_alias.to_lowercase();
    catalog.tools.iter().find(|t| {
        t.id.to_lowercase() == needle || t.aliases.iter().any(|a| a.to_lowercase() == needle)
    })
}

/// 版本约束匹配。
///
/// 支持三种形态（够用且不会误匹配）：
/// - **精确**：`24.19.0` 只匹配 `24.19.0`（前缀 `v` 会被忽略）
/// - **前缀**：`24` 匹配 `24.19.0`，**但不匹配** `240.1.0` —— 按点分段比较，不是字符串前缀
/// - **主版本带后缀**：`8` 匹配 `8u491`（Oracle JDK 的真实形状是 `8u491` 而不是 `8.491`）。
///   规则是：约束只有一段、而版本首段以「约束 + 非数字」开头时算匹配。
///   `8` **不**匹配 `80u1`（因为 `8` 后面是数字而不是非数字），也**不**匹配 `18u1`。
#[must_use]
pub fn version_matches(constraint: &str, version: &str) -> bool {
    let c = trim_v(constraint);
    let v = trim_v(version);
    if c == v {
        return true;
    }
    let c_parts: Vec<&str> = c.split('.').collect();
    let v_parts: Vec<&str> = v.split('.').collect();

    if c_parts.len() == 1 {
        // `8` vs `8u491`：首段以约束开头，且紧随的字符不是数字。
        if let Some(first) = v_parts.first()
            && let Some(rest) = first.strip_prefix(c)
            && let Some(ch) = rest.chars().next()
            && !ch.is_ascii_digit()
        {
            return true;
        }
    }

    if c_parts.len() >= v_parts.len() {
        // 约束比版本更细，只能精确匹配（上面已经比过了）。
        return false;
    }
    c_parts.iter().zip(v_parts.iter()).all(|(a, b)| a == b)
}

fn trim_v(s: &str) -> &str {
    s.strip_prefix('v').unwrap_or(s)
}

/// 解析出一个可安装的具体版本。
///
/// **生效的许可证结论 = 工具层与 recipe 层里更严格的那个。**
pub fn resolve(
    catalog: &Catalog,
    id_or_alias: &str,
    constraint: &str,
    platform: &str,
) -> Result<ResolvedRecipe, ResolveError> {
    let tool = find_tool(catalog, id_or_alias)
        .ok_or_else(|| ResolveError::UnknownTool(id_or_alias.to_owned()))?;

    let on_platform: Vec<_> = tool
        .recipes
        .iter()
        .filter(|r| r.platform == platform)
        .collect();

    if on_platform.is_empty() {
        return Err(ResolveError::NoPlatform {
            tool: tool.id.clone(),
            platform: platform.to_owned(),
        });
    }

    let recipe = on_platform
        .iter()
        .find(|r| version_matches(constraint, &r.version))
        .ok_or_else(|| ResolveError::NoVersion {
            tool: tool.id.clone(),
            constraint: constraint.to_owned(),
            known: on_platform
                .iter()
                .map(|r| r.version.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        })?;

    let (redistribution, licence_name, licence_notes) =
        effective_licence(tool, recipe.licence.as_ref());

    Ok(ResolvedRecipe {
        tool_id: tool.id.clone(),
        display_name: tool.display_name.clone(),
        version: recipe.version.clone(),
        platform: recipe.platform.clone(),
        url: render_url(&recipe.url, &recipe.version, platform),
        checksum: recipe.checksum.clone(),
        archive: recipe.archive,
        layout: recipe.layout.clone(),
        redistribution,
        licence_name,
        licence_notes,
    })
}

/// 工具层与 recipe 层合并后的许可证结论。
///
/// **只收紧，不放宽。**
#[must_use]
pub fn effective_licence(
    tool: &Tool,
    recipe_licence: Option<&crate::model::ToolLicence>,
) -> (Redistribution, String, String) {
    let Some(recipe_licence) = recipe_licence else {
        return (
            tool.licence.redistribution,
            tool.licence.spdx_or_name.clone(),
            tool.licence.notes.clone(),
        );
    };

    let redistribution = tool
        .licence
        .redistribution
        .more_restrictive(recipe_licence.redistribution);

    // 名字与原因都保留两边 —— 用户需要知道"是这个版本的特殊规则拦住了我"。
    let licence_name = if tool.licence.spdx_or_name == recipe_licence.spdx_or_name {
        tool.licence.spdx_or_name.clone()
    } else {
        format!(
            "{}（本版本适用：{}）",
            tool.licence.spdx_or_name, recipe_licence.spdx_or_name
        )
    };

    let notes = match (
        tool.licence.notes.is_empty(),
        recipe_licence.notes.is_empty(),
    ) {
        (true, true) => String::new(),
        (false, true) => tool.licence.notes.clone(),
        (true, false) => recipe_licence.notes.clone(),
        (false, false) => format!("{}；{}", tool.licence.notes, recipe_licence.notes),
    };

    (redistribution, licence_name, notes)
}

/// 支持的占位符修饰符。
///
/// **为什么需要修饰符**：Temurin 的版本串是 `21.0.12.1+1`，而它在两处出现时形状不同 ——
/// 下载路径里的 `+` 必须编码成 `%2B`，文件名里的 `+` 被上游换成了 `_`。
/// 这不是我们发明的复杂度，是上游的实际形状；用命名修饰符如实表达它，
/// 比在模板里塞两套替换规则更容易看出哪里可能写错。
const MODIFIER_URLENCODED: &str = "urlencoded";
const MODIFIER_FILENAME: &str = "filename";

/// 把一个占位符的值按修饰符变换。
fn apply_modifier(value: &str, modifier: Option<&str>) -> Option<String> {
    match modifier {
        None => Some(value.to_owned()),
        Some(MODIFIER_URLENCODED) => Some(
            value
                .replace('%', "%25")
                .replace('+', "%2B")
                .replace(' ', "%20"),
        ),
        Some(MODIFIER_FILENAME) => Some(value.replace('+', "_")),
        Some(_) => None,
    }
}

/// 把 `{version}` / `{platform}` / `{version:urlencoded}` 之类的占位符填进模板。
///
/// **未知占位符或未知修饰符会让 [`render_url`] 原样留下它** ——
/// 静默替换成空串会变成一个 404，而 404 会被误读成"上游没这个版本"。
/// 校验阶段（[`crate::validate`]）负责把这种模板挡在加载之前。
#[must_use]
pub fn render_url(template: &str, version: &str, platform: &str) -> String {
    let mut out = String::with_capacity(template.len() + 16);
    let mut rest = template;

    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let Some(end_rel) = rest[start..].find('}') else {
            // 没有闭合的 `{`：原样保留，让校验去报错。
            out.push_str(&rest[start..]);
            return out;
        };
        let end = start + end_rel;
        let token = &rest[start + 1..end];
        let (name, modifier) = match token.split_once(':') {
            Some((n, m)) => (n, Some(m)),
            None => (token, None),
        };

        let value = match name {
            "version" => apply_modifier(version, modifier),
            "platform" => apply_modifier(platform, modifier),
            _ => None,
        };

        match value {
            Some(v) => out.push_str(&v),
            // 未知占位符/修饰符：原样保留，便于校验与诊断发现它。
            None => out.push_str(&rest[start..=end]),
        }
        rest = &rest[end + 1..];
    }

    out.push_str(rest);
    out
}

/// 模板里出现过的全部占位符（含修饰符，如 `version:urlencoded`）。校验与诊断用。
#[must_use]
pub fn placeholders(template: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let Some(end_rel) = rest[start..].find('}') else {
            break;
        };
        let end = start + end_rel;
        out.push(rest[start + 1..end].to_owned());
        rest = &rest[end + 1..];
    }
    out
}

/// 一个占位符是否是本版本认识的。
#[must_use]
pub fn placeholder_is_supported(token: &str) -> bool {
    let (name, modifier) = match token.split_once(':') {
        Some((n, m)) => (n, Some(m)),
        None => (token, None),
    };
    matches!(name, "version" | "platform")
        && matches!(
            modifier,
            None | Some(MODIFIER_URLENCODED) | Some(MODIFIER_FILENAME)
        )
}

/// 某个平台上所有**可安装**的版本（按 recipe 里给出的顺序）。
#[must_use]
pub fn installable_versions(
    catalog: &Catalog,
    id_or_alias: &str,
    platform: &str,
) -> Vec<(String, Redistribution)> {
    let Some(tool) = find_tool(catalog, id_or_alias) else {
        return Vec::new();
    };
    tool.recipes
        .iter()
        .filter(|r| r.platform == platform)
        .map(|r| {
            let (redistribution, _, _) = effective_licence(tool, r.licence.as_ref());
            (r.version.clone(), redistribution)
        })
        .collect()
}

/// 一个 recipe 的布局是否描述了任何命令。空布局意味着装完什么也敲不了。
#[must_use]
pub fn layout_exposes_anything(layout: &Layout) -> bool {
    !layout.bin.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::licence::ArchiveFormat;
    use crate::model::{Checksum, Recipe, ToolLicence, VersionSource};

    fn recipe(version: &str, licence: Option<ToolLicence>) -> Recipe {
        Recipe {
            version: version.to_owned(),
            platform: PLATFORM_WINDOWS_X64.to_owned(),
            url: format!("https://example.invalid/node-v{{version}}-win-x64.zip#{version}"),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "a".repeat(64),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout::default(),
            licence,
        }
    }

    fn tool(id: &str, redistribution: Redistribution, recipes: Vec<Recipe>) -> Tool {
        Tool {
            id: id.to_owned(),
            display_name: id.to_owned(),
            aliases: vec![],
            description: String::new(),
            homepage: None,
            licence: ToolLicence {
                redistribution,
                spdx_or_name: "MIT".to_owned(),
                notes: "工具层原因".to_owned(),
                url: None,
            },
            version_sources: vec![VersionSource::Template {
                url_template: "https://example.invalid/{version}".to_owned(),
            }],
            recipes,
        }
    }

    fn catalog(tools: Vec<Tool>) -> Catalog {
        Catalog {
            schema_version: 1,
            name: "t".to_owned(),
            tools,
        }
    }

    #[test]
    fn exact_version_matches() {
        assert!(version_matches("24.19.0", "24.19.0"));
        assert!(version_matches("v24.19.0", "24.19.0"));
        assert!(!version_matches("24.19.1", "24.19.0"));
    }

    #[test]
    fn prefix_matches_by_dot_segment_not_by_string() {
        assert!(version_matches("24", "24.19.0"));
        assert!(version_matches("24.19", "24.19.0"));
        // 关键反例：字符串前缀会误匹配这个。
        assert!(!version_matches("24", "240.1.0"));
        assert!(!version_matches("24.1", "24.19.0"));
    }

    #[test]
    fn constraint_finer_than_version_never_matches() {
        assert!(!version_matches("24.19.0.1", "24.19.0"));
    }

    #[test]
    fn major_only_constraint_matches_a_letter_suffixed_version() {
        // Oracle JDK 的真实形状是 `8u491` 而不是 `8.491`。
        assert!(version_matches("8", "8u491"));
        assert!(version_matches("8u491", "8u491"));
        // 反例：`8` 不能匹配 `80u1`，也不能匹配 `18u1`。
        assert!(!version_matches("8", "80u1"));
        assert!(!version_matches("8", "18u1"));
        assert!(!version_matches("8", "9u1"));
    }

    #[test]
    fn find_tool_resolves_aliases_case_insensitively() {
        let mut t = tool("node", Redistribution::Allowed, vec![]);
        t.aliases = vec!["nodejs".to_owned()];
        let c = catalog(vec![t]);
        assert!(find_tool(&c, "node").is_some());
        assert!(find_tool(&c, "NODEJS").is_some());
        assert!(find_tool(&c, "NodeJS").is_some());
        assert!(find_tool(&c, "nope").is_none());
    }

    #[test]
    fn unknown_tool_is_a_distinct_error() {
        let c = catalog(vec![]);
        let err = resolve(&c, "node", "24", PLATFORM_WINDOWS_X64).expect_err("应当失败");
        assert!(matches!(err, ResolveError::UnknownTool(_)), "{err:?}");
        assert!(err.to_string().contains("node"), "{err}");
    }

    #[test]
    fn missing_platform_lists_the_platform() {
        let c = catalog(vec![tool(
            "node",
            Redistribution::Allowed,
            vec![recipe("24.19.0", None)],
        )]);
        let err = resolve(&c, "node", "24", PLATFORM_WINDOWS_ARM64).expect_err("应当失败");
        assert!(matches!(err, ResolveError::NoPlatform { .. }), "{err:?}");
        assert!(err.to_string().contains("windows-arm64"), "{err}");
    }

    #[test]
    fn missing_version_lists_what_does_exist() {
        // 错误信息里带上已知版本，用户就不用去猜。
        let c = catalog(vec![tool(
            "node",
            Redistribution::Allowed,
            vec![recipe("24.19.0", None), recipe("22.11.0", None)],
        )]);
        let err = resolve(&c, "node", "99", PLATFORM_WINDOWS_X64).expect_err("应当失败");
        let text = err.to_string();
        assert!(text.contains("24.19.0"), "{text}");
        assert!(text.contains("22.11.0"), "{text}");
        assert!(text.contains("99"), "{text}");
    }

    #[test]
    fn resolve_renders_the_url_and_keeps_the_checksum() {
        let c = catalog(vec![tool(
            "node",
            Redistribution::Allowed,
            vec![recipe("24.19.0", None)],
        )]);
        let r = resolve(&c, "node", "24", PLATFORM_WINDOWS_X64).expect("resolve");
        assert!(r.url.contains("24.19.0"), "{}", r.url);
        assert!(!r.url.contains("{version}"), "{}", r.url);
        assert_eq!(r.checksum.value.len(), 64);
    }

    #[test]
    fn recipe_licence_cannot_relax_a_prohibited_tool() {
        // **本模块最重要的用例**：一个"看起来可再分发"的 recipe
        // 不能洗白工具层的 prohibited 结论。
        let permissive = ToolLicence {
            redistribution: Redistribution::Allowed,
            spdx_or_name: "NFTC".to_owned(),
            notes: "本版本可再分发".to_owned(),
            url: None,
        };
        let c = catalog(vec![tool(
            "oracle-jdk",
            Redistribution::Prohibited,
            vec![recipe("8u491", Some(permissive))],
        )]);
        let r = resolve(&c, "oracle-jdk", "8", PLATFORM_WINDOWS_X64).expect("resolve");
        assert_eq!(r.redistribution, Redistribution::Prohibited);
        assert!(
            !crate::licence::evaluate(r.redistribution, &r.licence_name, &r.licence_notes)
                .is_allowed()
        );
    }

    #[test]
    fn recipe_licence_can_tighten_an_allowed_tool() {
        let stricter = ToolLicence {
            redistribution: Redistribution::Prohibited,
            spdx_or_name: "Oracle BCL".to_owned(),
            notes: "这个旧版本不可再分发".to_owned(),
            url: None,
        };
        let c = catalog(vec![tool(
            "oracle-jdk",
            Redistribution::Allowed,
            vec![recipe("8u491", Some(stricter))],
        )]);
        let r = resolve(&c, "oracle-jdk", "8", PLATFORM_WINDOWS_X64).expect("resolve");
        assert_eq!(r.redistribution, Redistribution::Prohibited);
    }

    #[test]
    fn merged_notes_keep_both_sides() {
        let recipe_licence = ToolLicence {
            redistribution: Redistribution::Conditional,
            spdx_or_name: "NFTC".to_owned(),
            notes: "本版本有条件".to_owned(),
            url: None,
        };
        let t = tool("oracle-jdk", Redistribution::Allowed, vec![]);
        let (_, name, notes) = effective_licence(&t, Some(&recipe_licence));
        assert!(notes.contains("工具层原因"), "{notes}");
        assert!(notes.contains("本版本有条件"), "{notes}");
        assert!(name.contains("NFTC"), "{name}");
        assert!(name.contains("MIT"), "{name}");
    }

    #[test]
    fn same_licence_name_is_not_duplicated_in_the_label() {
        let same = ToolLicence {
            redistribution: Redistribution::Allowed,
            spdx_or_name: "MIT".to_owned(),
            notes: String::new(),
            url: None,
        };
        let t = tool("node", Redistribution::Allowed, vec![]);
        let (_, name, _) = effective_licence(&t, Some(&same));
        assert_eq!(name, "MIT");
    }

    #[test]
    fn installable_versions_reports_the_licence_for_each() {
        let strict = ToolLicence {
            redistribution: Redistribution::Prohibited,
            spdx_or_name: "BCL".to_owned(),
            notes: "不可再分发".to_owned(),
            url: None,
        };
        let c = catalog(vec![tool(
            "oracle-jdk",
            Redistribution::Allowed,
            vec![recipe("21.0.1", None), recipe("8u491", Some(strict))],
        )]);
        let versions = installable_versions(&c, "oracle-jdk", PLATFORM_WINDOWS_X64);
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].1, Redistribution::Allowed);
        assert_eq!(versions[1].1, Redistribution::Prohibited);
    }

    #[test]
    fn placeholders_are_extracted_for_diagnostics() {
        assert_eq!(
            placeholders("https://x/{version}/{platform}.zip"),
            vec!["version".to_owned(), "platform".to_owned()]
        );
        assert!(placeholders("https://x/static.zip").is_empty());
        // 修饰符随名字一起取出，便于报错时说清是哪一个不支持。
        assert_eq!(
            placeholders("https://x/jdk-{version:urlencoded}/{version:filename}.zip"),
            vec![
                "version:urlencoded".to_owned(),
                "version:filename".to_owned()
            ]
        );
    }

    #[test]
    fn urlencoded_modifier_encodes_the_plus_sign() {
        // Temurin 的真实形状：路径里 `+` 必须编码，文件名里 `+` 变 `_`。
        let url = render_url(
            "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-{version:urlencoded}/OpenJDK21U-jdk_x64_windows_hotspot_{version:filename}.zip",
            "21.0.12.1+1",
            PLATFORM_WINDOWS_X64,
        );
        assert!(url.contains("jdk-21.0.12.1%2B1"), "{url}");
        assert!(url.contains("hotspot_21.0.12.1_1.zip"), "{url}");
        assert!(!url.contains('{'), "{url}");
        assert!(!url.contains('+'), "{url}");
    }

    #[test]
    fn unknown_placeholder_is_left_verbatim_not_blanked() {
        // 静默替换成空串会变成一个 404，而 404 会被误读成"上游没这个版本"。
        let url = render_url(
            "https://x/{arch}/{version}.zip",
            "1.0",
            PLATFORM_WINDOWS_X64,
        );
        assert!(url.contains("{arch}"), "{url}");
        assert!(url.contains("1.0"), "{url}");
    }

    #[test]
    fn unknown_modifier_is_left_verbatim() {
        let url = render_url(
            "https://x/{version:base64}.zip",
            "1.0",
            PLATFORM_WINDOWS_X64,
        );
        assert!(url.contains("{version:base64}"), "{url}");
    }

    #[test]
    fn unclosed_brace_does_not_panic_or_truncate() {
        let url = render_url("https://x/{version", "1.0", PLATFORM_WINDOWS_X64);
        assert_eq!(url, "https://x/{version");
    }

    #[test]
    fn placeholder_support_is_checkable() {
        assert!(placeholder_is_supported("version"));
        assert!(placeholder_is_supported("platform"));
        assert!(placeholder_is_supported("version:urlencoded"));
        assert!(placeholder_is_supported("version:filename"));
        assert!(!placeholder_is_supported("arch"));
        assert!(!placeholder_is_supported("version:base64"));
    }

    #[test]
    fn layout_exposes_anything_detects_empty_bin() {
        assert!(!layout_exposes_anything(&Layout::default()));
        let mut layout = Layout::default();
        layout.bin.insert("node".to_owned(), "node.exe".to_owned());
        assert!(layout_exposes_anything(&layout));
    }
}
