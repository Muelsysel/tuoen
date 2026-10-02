//! 内置种子 catalog 的加载。
//!
//! 种子目录**编译进二进制**（`include_str!`），所以无网络用户至少能看见"我们认识哪些工具、
//! 每个工具的许可证是什么"。这与 `docs/DESIGN.md` 决策 19 一致：
//! 内置种子 + 可选远程签名索引。
//!
//! **种子目录里故意含不可再分发的条目**（Oracle JDK 8、MSVC）——
//! 它们不是脏数据，是这个目录的测试用例，也是对用户的说明：
//! "我们知道它存在，但你不能通过 tuoen 装它，原因如下。"

use crate::error::{ManifestError, ValidationIssue};
use crate::model::Catalog;
use crate::validate::validate_catalog;

/// 内置种子 catalog 的原文。
pub const SEED_TOML: &str = include_str!("../catalogs/seed.toml");

/// 解析并校验内置种子 catalog。
///
/// **校验失败会返回错误而不是 panic** —— 种子目录是我们自己写的，
/// 但它仍然应当被当作外部输入对待：一个 panic 会让整个 CLI 不可用。
pub fn load_seed() -> Result<Catalog, ManifestError> {
    let catalog: Catalog = toml::from_str(SEED_TOML)?;
    let issues = validate_catalog(&catalog);
    if issues.is_empty() {
        Ok(catalog)
    } else {
        Err(ManifestError::Invalid(issues))
    }
}

/// 解析并校验任意一段 catalog TOML。
pub fn load_str(text: &str) -> Result<Catalog, ManifestError> {
    let catalog: Catalog = toml::from_str(text)?;
    let issues = validate_catalog(&catalog);
    if issues.is_empty() {
        Ok(catalog)
    } else {
        Err(ManifestError::Invalid(issues))
    }
}

/// 只校验，不要求成功。给"显示全部问题"这类场景用。
pub fn lint_str(text: &str) -> Result<Vec<ValidationIssue>, ManifestError> {
    let catalog: Catalog = toml::from_str(text)?;
    Ok(validate_catalog(&catalog))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::licence::Redistribution;
    use crate::resolve::{PLATFORM_WINDOWS_X64, find_tool, installable_versions, resolve};

    #[test]
    fn seed_catalog_parses_and_validates() {
        // 这条测试是种子目录的守门人：任何字段写错都会在这里炸，
        // 而且错误信息会带字段路径。
        let catalog = load_seed().unwrap_or_else(|e| panic!("种子目录无效：\n{e}"));
        assert_eq!(catalog.schema_version, 1);
        assert!(catalog.tools.len() >= 4, "种子目录太小");
    }

    #[test]
    fn seed_contains_node_and_temurin_as_installable() {
        let catalog = load_seed().expect("seed");
        for id in ["node", "temurin"] {
            let tool = find_tool(&catalog, id).unwrap_or_else(|| panic!("缺少 {id}"));
            assert_eq!(
                tool.licence.redistribution,
                Redistribution::Allowed,
                "{id} 应当是 allowed"
            );
            assert!(
                !tool.recipes.is_empty(),
                "{id} 必须至少有一个 recipe，否则'可装'是假的"
            );
        }
    }

    #[test]
    fn seed_contains_a_prohibited_example_so_the_gate_is_demonstrable() {
        // ticket #3 的验收要求：故意含一个不可再分发的反例。
        let catalog = load_seed().expect("seed");
        let prohibited: Vec<_> = catalog
            .tools
            .iter()
            .filter(|t| t.licence.redistribution == Redistribution::Prohibited)
            .collect();
        assert!(
            !prohibited.is_empty(),
            "种子目录必须含一个不可再分发的反例来证明门禁真的会拦"
        );
        for tool in prohibited {
            assert!(
                !tool.licence.notes.trim().is_empty(),
                "{} 是不可再分发却没写原因",
                tool.id
            );
        }
    }

    #[test]
    fn node_recipes_have_real_looking_sha256_and_render_without_placeholders() {
        let catalog = load_seed().expect("seed");
        let versions = installable_versions(&catalog, "node", PLATFORM_WINDOWS_X64);
        assert!(!versions.is_empty());
        for (version, redistribution) in versions {
            assert_eq!(redistribution, Redistribution::Allowed);
            let r = resolve(&catalog, "node", &version, PLATFORM_WINDOWS_X64)
                .unwrap_or_else(|e| panic!("{version}: {e}"));
            assert_eq!(r.checksum.value.len(), 64, "{version}");
            assert!(
                !r.url.contains('{'),
                "{version} 的 URL 还有占位符：{}",
                r.url
            );
            assert!(
                !r.url.contains("+"),
                "{version} 的 URL 还有未编码的 +：{}",
                r.url
            );
        }
    }

    #[test]
    fn temurin_recipe_encodes_plus_in_path_and_underscore_in_filename() {
        // 上游的实际形状：release_name 是 `21.0.12.1+1`，
        // 下载路径里是 `%2B`，文件名里是 `_`。
        let catalog = load_seed().expect("seed");
        let r = resolve(&catalog, "temurin", "21.0.12.1+1", PLATFORM_WINDOWS_X64).expect("resolve");
        assert!(r.url.contains("jdk-21.0.12.1%2B1"), "{}", r.url);
        assert!(
            r.url
                .contains("OpenJDK21U-jdk_x64_windows_hotspot_21.0.12.1_1.zip"),
            "{}",
            r.url
        );
        assert!(!r.url.contains('{'), "{}", r.url);
    }

    #[test]
    fn temurin_alias_jdk_resolves_to_the_same_tool() {
        let catalog = load_seed().expect("seed");
        let a = resolve(&catalog, "temurin", "21", PLATFORM_WINDOWS_X64).expect("by id");
        let b = resolve(&catalog, "jdk", "21", PLATFORM_WINDOWS_X64).expect("by alias");
        assert_eq!(a, b);
    }

    #[test]
    fn prohibited_seed_entries_are_rejected_by_the_gate_with_a_specific_reason() {
        // 这是 ticket #3 的核心验收：门禁真的会拦，且原因具体。
        let catalog = load_seed().expect("seed");
        for id in ["oracle-jdk", "msvc"] {
            let tool = find_tool(&catalog, id).unwrap_or_else(|| panic!("缺少 {id}"));
            let verdict = crate::licence::evaluate(
                tool.licence.redistribution,
                &tool.licence.spdx_or_name,
                &tool.licence.notes,
            );
            assert!(!verdict.is_allowed(), "{id} 应当被门禁拦下");
            match verdict {
                crate::licence::GateVerdict::Rejected { reason } => {
                    assert!(
                        reason.contains(&tool.licence.spdx_or_name),
                        "{id}: {reason}"
                    );
                    assert!(reason.len() > 20, "{id} 的拒绝原因太短：{reason}");
                    assert!(!reason.contains("许可问题"), "{id}: {reason}");
                }
                other => panic!("{id} 应当被拒绝，实际：{other:?}"),
            }
        }
    }

    #[test]
    fn msvc_is_modelled_as_an_out_of_band_prerequisite_not_a_package() {
        // 它不是包：没有 recipe，描述里说清了要用户自己以管理员身份装。
        let catalog = load_seed().expect("seed");
        let msvc = find_tool(&catalog, "msvc").expect("msvc");
        assert!(
            msvc.recipes.is_empty(),
            "MSVC 不该有 recipe —— 它是带外管理员前置条件，不是可管理包"
        );
        assert!(
            msvc.description.contains("管理员"),
            "描述必须说清需要管理员：{}",
            msvc.description
        );
    }

    #[test]
    fn seed_has_no_duplicate_ids_or_aliases() {
        // 校验器会报重复，这里断言种子目录本身是干净的。
        let catalog = load_seed().expect("seed");
        let issues = validate_catalog(&catalog);
        assert!(issues.is_empty(), "{issues:#?}");
    }

    #[test]
    fn malformed_catalog_reports_field_paths_not_a_panic() {
        let bad = r#"
schema_version = 1
[[tool]]
id = "BAD ID"
display_name = ""
licence = { redistribution = "allowed", spdx_or_name = "" }
"#;
        let err = load_str(bad).expect_err("应当失败");
        let text = err.to_string();
        assert!(text.contains("tool[0].id"), "{text}");
        assert!(text.contains("tool[0].display_name"), "{text}");
        assert!(text.contains("tool[0].licence.spdx_or_name"), "{text}");
    }

    #[test]
    fn toml_syntax_error_is_reported_as_such() {
        let err = load_str("this is = = not toml").expect_err("应当失败");
        assert!(matches!(err, ManifestError::Toml { .. }), "{err:?}");
    }
}
