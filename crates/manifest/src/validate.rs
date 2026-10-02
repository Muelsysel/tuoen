//! schema 校验。
//!
//! **设计要点：一次收集全部问题。** 用户改了 6 个字段里的 5 个，不该需要跑 5 次
//! 才知道第 6 个也错了。每条问题都带**字段路径**，所以错误是可定位的。
//!
//! 校验发生在 `Deserialize` **之后**而不是之中：把校验塞进 `Deserialize` 会
//! 让"解析成功但语义错"这类输入只能报第一个问题。

use std::collections::HashSet;

use crate::error::ValidationIssue;
use crate::licence::ArchiveFormat;
use crate::model::{Catalog, Checksum, Layout, Recipe, Tool, VersionSource};

/// 目前支持的 schema 版本。
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// 校验整个 catalog。返回**全部**问题（可能为空）。
#[must_use]
pub fn validate_catalog(catalog: &Catalog) -> Vec<ValidationIssue> {
    let mut issues = Vec::new();

    if catalog.schema_version != SUPPORTED_SCHEMA_VERSION {
        issues.push(ValidationIssue::new(
            "schema_version",
            format!(
                "不支持的版本 {}，本版本只认识 {SUPPORTED_SCHEMA_VERSION}",
                catalog.schema_version
            ),
        ));
    }

    if catalog.tools.is_empty() {
        issues.push(ValidationIssue::new("tool", "至少要有一个工具"));
    }

    let mut seen_ids = HashSet::new();
    let mut seen_aliases: HashSet<String> = HashSet::new();

    for (i, tool) in catalog.tools.iter().enumerate() {
        let base = format!("tool[{i}]");
        validate_tool(tool, &base, &mut issues);

        if !seen_ids.insert(tool.id.to_lowercase()) {
            issues.push(ValidationIssue::new(
                format!("{base}.id"),
                format!("工具 id `{}` 重复", tool.id),
            ));
        }

        for alias in &tool.aliases {
            if !seen_aliases.insert(alias.to_lowercase()) {
                issues.push(ValidationIssue::new(
                    format!("{base}.aliases"),
                    format!("别名 `{alias}` 已被别的工具占用（别名必须全局唯一）"),
                ));
            }
        }
    }

    issues
}

fn validate_tool(tool: &Tool, base: &str, issues: &mut Vec<ValidationIssue>) {
    if tool.id.trim().is_empty() {
        issues.push(ValidationIssue::new(format!("{base}.id"), "不能为空"));
    } else if !is_valid_id(&tool.id) {
        issues.push(ValidationIssue::new(
            format!("{base}.id"),
            format!(
                "`{}` 不是合法的 id：只允许小写字母、数字和 `-`，且不能以 `-` 开头或结尾",
                tool.id
            ),
        ));
    }

    if tool.display_name.trim().is_empty() {
        issues.push(ValidationIssue::new(
            format!("{base}.display_name"),
            "不能为空",
        ));
    }

    if tool.licence.spdx_or_name.trim().is_empty() {
        issues.push(ValidationIssue::new(
            format!("{base}.licence.spdx_or_name"),
            "不能为空 —— 许可证标识是模型的一等字段，不允许留空",
        ));
    }

    // 不可再分发的工具**必须**写清原因，否则用户看到的拒绝信息会是无用的。
    if tool.licence.redistribution == crate::licence::Redistribution::Prohibited
        && tool.licence.notes.trim().is_empty()
    {
        issues.push(ValidationIssue::new(
            format!("{base}.licence.notes"),
            "结论是 prohibited 时必须写清原因 —— 拒绝安装而不给出具体原因等于没给信息",
        ));
    }

    for (j, source) in tool.version_sources.iter().enumerate() {
        let path = format!("{base}.version_source[{j}]");
        match source {
            VersionSource::Api { index_url, .. } | VersionSource::RemoteIndex { index_url } => {
                if index_url.trim().is_empty() {
                    issues.push(ValidationIssue::new(
                        format!("{path}.index_url"),
                        "不能为空",
                    ));
                } else if !index_url.starts_with("https://") {
                    issues.push(ValidationIssue::new(
                        format!("{path}.index_url"),
                        format!(
                            "必须是 https:// —— 明文 http 的版本索引可以被中间人替换：{index_url}"
                        ),
                    ));
                }
            }
            VersionSource::Template { url_template } => {
                if !url_template.starts_with("https://") {
                    issues.push(ValidationIssue::new(
                        format!("{path}.url_template"),
                        format!("必须是 https://：{url_template}"),
                    ));
                }
                if !url_template.contains("{version}") && !url_template.contains("{version:") {
                    issues.push(ValidationIssue::new(
                        format!("{path}.url_template"),
                        "模板必须含 `{version}`，否则所有版本会解析到同一个地址",
                    ));
                }
                for token in crate::resolve::placeholders(url_template) {
                    if !crate::resolve::placeholder_is_supported(&token) {
                        issues.push(ValidationIssue::new(
                            format!("{path}.url_template"),
                            format!("不认识的占位符 `{{{token}}}`"),
                        ));
                    }
                }
            }
        }
    }

    let mut seen_recipe_keys = HashSet::new();
    for (k, recipe) in tool.recipes.iter().enumerate() {
        let path = format!("{base}.recipe[{k}]");
        validate_recipe(recipe, &path, issues);

        // 同一个 (version, platform) 只能有一个 recipe，否则"装哪个"取决于顺序。
        let key = (recipe.version.clone(), recipe.platform.clone());
        if !seen_recipe_keys.insert(key.clone()) {
            issues.push(ValidationIssue::new(
                format!("{path}.version"),
                format!("版本 `{}` 在平台 `{}` 上重复定义", key.0, key.1),
            ));
        }
    }
}

fn validate_recipe(recipe: &Recipe, path: &str, issues: &mut Vec<ValidationIssue>) {
    if recipe.version.trim().is_empty() {
        issues.push(ValidationIssue::new(format!("{path}.version"), "不能为空"));
    }

    if !is_valid_platform(&recipe.platform) {
        issues.push(ValidationIssue::new(
            format!("{path}.platform"),
            format!(
                "`{}` 不是合法平台：只允许 `windows-x64` / `windows-arm64`（V1 只发 Windows）",
                recipe.platform
            ),
        ));
    }

    if !recipe.url.starts_with("https://") {
        issues.push(ValidationIssue::new(
            format!("{path}.url"),
            format!("必须是 https://：{}", recipe.url),
        ));
    }
    if !recipe.url.contains("{version}") && !recipe.url.contains("{version:") {
        issues.push(ValidationIssue::new(
            format!("{path}.url"),
            "URL 模板必须含 `{version}`，否则所有版本会下载同一个文件",
        ));
    }

    // 未知占位符/修饰符必须在这里挡住：渲染时会原样保留它，于是变成 404，
    // 而 404 会被误读成"上游没这个版本"。
    for token in crate::resolve::placeholders(&recipe.url) {
        if !crate::resolve::placeholder_is_supported(&token) {
            issues.push(ValidationIssue::new(
                format!("{path}.url"),
                format!(
                    "不认识的占位符 `{{{token}}}`：只支持 `{{version}}`、`{{platform}}`，\
                     以及修饰符 `:urlencoded` / `:filename`"
                ),
            ));
        }
    }

    validate_checksum(&recipe.checksum, &format!("{path}.checksum"), issues);
    validate_layout(&recipe.layout, &format!("{path}.layout"), issues);

    if recipe.archive == ArchiveFormat::SevenZip {
        // 不是错误，但必须让作者知道这条路要内置库 —— 系统 tar 读不了 7z。
        issues.push(ValidationIssue::new(
            format!("{path}.archive"),
            "7z 需要内置解压库（系统 tar.exe 读不了它）—— 确认这是有意的",
        ));
    }
}

fn validate_checksum(checksum: &Checksum, path: &str, issues: &mut Vec<ValidationIssue>) {
    let algorithm = checksum.algorithm.to_lowercase();
    if algorithm != "sha256" {
        issues.push(ValidationIssue::new(
            format!("{path}.algorithm"),
            format!("只支持 sha256，收到 `{}`", checksum.algorithm),
        ));
    }
    let value = checksum.value.trim();
    if value.len() != 64 {
        issues.push(ValidationIssue::new(
            format!("{path}.value"),
            format!("sha256 应当是 64 个十六进制字符，收到 {} 个", value.len()),
        ));
    } else if !value.chars().all(|c| c.is_ascii_hexdigit()) {
        issues.push(ValidationIssue::new(
            format!("{path}.value"),
            "sha256 里出现了非十六进制字符",
        ));
    }
}

fn validate_layout(layout: &Layout, path: &str, issues: &mut Vec<ValidationIssue>) {
    for (name, target) in &layout.bin {
        if name.trim().is_empty() {
            issues.push(ValidationIssue::new(
                format!("{path}.bin"),
                "存在空的命令名",
            ));
        }
        if target.trim().is_empty() {
            issues.push(ValidationIssue::new(
                format!("{path}.bin.{name}"),
                "目标路径不能为空",
            ));
            continue;
        }
        // 解压出来的路径必须留在解压根内 —— 这是 Zip Slip 的第一道闸。
        // 真正的解压检查在 ticket #5，但 manifest 层能提前拦掉一部分。
        let looks_absolute = target.starts_with('/')
            || target.starts_with('\\')
            || target.contains(":\\")
            || target.contains(":/");
        if looks_absolute {
            issues.push(ValidationIssue::new(
                format!("{path}.bin.{name}"),
                format!("目标必须是归档内的相对路径，不能是绝对路径：{target}"),
            ));
        }
        if target.split(['/', '\\']).any(|part| part == "..") {
            issues.push(ValidationIssue::new(
                format!("{path}.bin.{name}"),
                format!("目标不得含 `..`（路径穿越）：{target}"),
            ));
        }
    }
}

fn is_valid_id(id: &str) -> bool {
    if id.is_empty() || id.starts_with('-') || id.ends_with('-') {
        return false;
    }
    id.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_valid_platform(platform: &str) -> bool {
    matches!(platform, "windows-x64" | "windows-arm64")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::licence::Redistribution;
    use crate::model::{Catalog, ToolLicence};

    fn tool(id: &str) -> Tool {
        Tool {
            id: id.to_owned(),
            display_name: "X".to_owned(),
            aliases: Vec::new(),
            description: String::new(),
            homepage: None,
            licence: ToolLicence {
                redistribution: Redistribution::Allowed,
                spdx_or_name: "MIT".to_owned(),
                notes: String::new(),
                url: None,
            },
            version_sources: Vec::new(),
            recipes: Vec::new(),
        }
    }

    fn catalog(tools: Vec<Tool>) -> Catalog {
        Catalog {
            schema_version: SUPPORTED_SCHEMA_VERSION,
            name: "test".to_owned(),
            tools,
        }
    }

    #[test]
    fn a_healthy_catalog_has_no_issues() {
        let issues = validate_catalog(&catalog(vec![tool("node")]));
        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn wrong_schema_version_is_reported_with_both_numbers() {
        let mut c = catalog(vec![tool("node")]);
        c.schema_version = 99;
        let issues = validate_catalog(&c);
        let text = issues[0].to_string();
        assert!(text.contains("99"), "{text}");
        assert!(text.contains("1"), "{text}");
    }

    #[test]
    fn empty_catalog_is_an_error() {
        let issues = validate_catalog(&catalog(vec![]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, "tool");
    }

    #[test]
    fn bad_id_is_reported_with_the_rule() {
        let issues = validate_catalog(&catalog(vec![tool("Node JS")]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, "tool[0].id");
        assert!(
            issues[0].message.contains("小写字母"),
            "{}",
            issues[0].message
        );
    }

    #[test]
    fn duplicate_ids_are_caught_case_insensitively() {
        let issues = validate_catalog(&catalog(vec![tool("node"), tool("NODE")]));
        assert!(
            issues.iter().any(|i| i.message.contains("重复")),
            "{issues:#?}"
        );
    }

    #[test]
    fn duplicate_aliases_across_tools_are_caught() {
        let mut a = tool("node");
        a.aliases = vec!["js".to_owned()];
        let mut b = tool("deno");
        b.aliases = vec!["js".to_owned()];
        let issues = validate_catalog(&catalog(vec![a, b]));
        assert!(
            issues.iter().any(|i| i.message.contains("别名")),
            "{issues:#?}"
        );
    }

    #[test]
    fn prohibited_without_notes_is_rejected_by_validation() {
        // 拒绝安装却不给原因 = 没给信息。这是 spec 明确要求的。
        let mut t = tool("oracle-jdk");
        t.licence.redistribution = Redistribution::Prohibited;
        t.licence.notes = String::new();
        let issues = validate_catalog(&catalog(vec![t]));
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].path, "tool[0].licence.notes");
    }

    #[test]
    fn prohibited_with_notes_passes() {
        let mut t = tool("oracle-jdk");
        t.licence.redistribution = Redistribution::Prohibited;
        t.licence.notes = "Oracle JDK 8/11/17 的许可不允许再分发。".to_owned();
        let issues = validate_catalog(&catalog(vec![t]));
        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn http_urls_are_rejected_everywhere() {
        let mut t = tool("node");
        t.version_sources = vec![VersionSource::Template {
            url_template: "http://x/{version}/".to_owned(),
        }];
        let issues = validate_catalog(&catalog(vec![t]));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains("https"), "{}", issues[0].message);
    }

    #[test]
    fn template_without_version_placeholder_is_rejected_with_the_reason() {
        let mut t = tool("node");
        t.version_sources = vec![VersionSource::Template {
            url_template: "https://x/static/".to_owned(),
        }];
        let issues = validate_catalog(&catalog(vec![t]));
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].message.contains("同一个地址"),
            "{}",
            issues[0].message
        );
    }

    #[test]
    fn every_problem_is_reported_at_once() {
        // 用户改了 6 个字段里的 5 个，不该需要跑 5 次。
        let mut t = tool("BAD ID");
        t.display_name = String::new();
        t.licence.spdx_or_name = String::new();
        let issues = validate_catalog(&catalog(vec![t]));
        assert!(issues.len() >= 3, "只报了一个：{issues:#?}");
    }

    #[test]
    fn bad_sha256_length_is_reported_with_the_actual_length() {
        let mut t = tool("node");
        t.recipes = vec![Recipe {
            version: "1".to_owned(),
            platform: "windows-x64".to_owned(),
            url: "https://x/{version}".to_owned(),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "abc".to_owned(),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout::default(),
            licence: None,
        }];
        let issues = validate_catalog(&catalog(vec![t]));
        assert_eq!(issues.len(), 1);
        assert!(issues[0].message.contains('3'), "{}", issues[0].message);
    }

    #[test]
    fn path_traversal_in_bin_is_rejected() {
        let mut t = tool("node");
        let mut bin = std::collections::BTreeMap::new();
        bin.insert("node".to_owned(), "../evil.exe".to_owned());
        t.recipes = vec![Recipe {
            version: "1".to_owned(),
            platform: "windows-x64".to_owned(),
            url: "https://x/{version}".to_owned(),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "a".repeat(64),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout {
                strip_components: 1,
                bin,
                home: None,
            },
            licence: None,
        }];
        let issues = validate_catalog(&catalog(vec![t]));
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].message.contains("路径穿越"),
            "{}",
            issues[0].message
        );
    }

    #[test]
    fn absolute_path_in_bin_is_rejected() {
        let mut t = tool("node");
        let mut bin = std::collections::BTreeMap::new();
        bin.insert("node".to_owned(), r"C:\Windows\System32\cmd.exe".to_owned());
        t.recipes = vec![Recipe {
            version: "1".to_owned(),
            platform: "windows-x64".to_owned(),
            url: "https://x/{version}".to_owned(),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "a".repeat(64),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout {
                strip_components: 0,
                bin,
                home: None,
            },
            licence: None,
        }];
        let issues = validate_catalog(&catalog(vec![t]));
        assert_eq!(issues.len(), 1);
        assert!(
            issues[0].message.contains("绝对路径"),
            "{}",
            issues[0].message
        );
    }

    #[test]
    fn duplicate_version_and_platform_is_rejected() {
        let recipe = Recipe {
            version: "1".to_owned(),
            platform: "windows-x64".to_owned(),
            url: "https://x/{version}".to_owned(),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "a".repeat(64),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout::default(),
            licence: None,
        };
        let mut t = tool("node");
        t.recipes = vec![recipe.clone(), recipe];
        let issues = validate_catalog(&catalog(vec![t]));
        assert!(
            issues.iter().any(|i| i.message.contains("重复定义")),
            "{issues:#?}"
        );
    }

    #[test]
    fn linux_platform_is_rejected_for_v1() {
        let mut t = tool("node");
        t.recipes = vec![Recipe {
            version: "1".to_owned(),
            platform: "linux-x64".to_owned(),
            url: "https://x/{version}".to_owned(),
            checksum: Checksum {
                algorithm: "sha256".to_owned(),
                value: "a".repeat(64),
            },
            archive: ArchiveFormat::Zip,
            layout: Layout::default(),
            licence: None,
        }];
        let issues = validate_catalog(&catalog(vec![t]));
        assert!(
            issues.iter().any(|i| i.message.contains("V1 只发 Windows")),
            "{issues:#?}"
        );
    }
}
