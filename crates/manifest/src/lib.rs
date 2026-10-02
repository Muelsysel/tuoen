//! tuoen 的 manifest 层：**两层 schema**。
//!
//! - **catalog** —— 工具元数据（名字、别名、描述、**许可证**、上游、版本来源）
//! - **recipe** —— 某个具体版本的安装步骤（URL 模板、校验、归档格式、解压布局、暴露方式）
//!
//! **为什么必须两层**（`docs/DESIGN.md` 决策 18）：许可证字段必须挂在"工具"层，
//! 但 Oracle 的规则**按版本分界**（JDK 21+ 为 NFTC 可镜像；JDK 8/11/17 不可再分发），
//! 两层的切分点正好落在"版本"上。
//!
//! ## 这一层要能回答三个问题
//!
//! 1. tuoen 认识哪些工具（[`Catalog`] / [`load_seed`]）
//! 2. 每个工具的许可证是什么（[`ToolLicence`] / [`Redistribution`]）
//! 3. 某个版本能不能装、从哪装、哈希是什么（[`resolve`]）
//!
//! **而不需要下载任何东西。** 下载在 L0 的下载层（ticket #4）。
//!
//! ## 一条不变量
//!
//! **recipe 层的许可证覆盖只能更严格，不能放宽** —— 见 [`licence`]。
//! 否则一个"看起来可再分发"的 recipe 就能绕开工具层的 `prohibited` 结论。

pub mod error;
pub mod licence;
pub mod model;
pub mod resolve;
pub mod seed;
pub mod validate;

pub use error::{ManifestError, ValidationIssue};
pub use licence::{ArchiveFormat, GateVerdict, Redistribution};
pub use model::{
    Catalog, Checksum, Layout, Recipe, ResolvedRecipe, Tool, ToolLicence, VersionSource,
};
pub use resolve::{
    PLATFORM_WINDOWS_ARM64, PLATFORM_WINDOWS_X64, ResolveError, effective_licence, find_tool,
    installable_versions, render_url, resolve, version_matches,
};
pub use seed::{SEED_TOML, load_seed, load_str};
pub use validate::{SUPPORTED_SCHEMA_VERSION, validate_catalog};

/// 对一个已解析的制品做许可证门禁判定。
///
/// 这是"能不能装"的**唯一**入口 —— 门禁不在别处再实现一次。
#[must_use]
pub fn gate(recipe: &ResolvedRecipe) -> GateVerdict {
    licence::evaluate(
        recipe.redistribution,
        &recipe.licence_name,
        &recipe.licence_notes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_rejects_a_prohibited_seed_entry() {
        // 从种子目录里走一遍完整链路：解析 → 解析版本 → 门禁拒绝。
        // 这是"用户真的会看到的路径"，比单独测 evaluate 更有说服力。
        let catalog = load_seed().expect("seed");
        let tool = find_tool(&catalog, "oracle-jdk").expect("oracle-jdk");
        let verdict = licence::evaluate(
            tool.licence.redistribution,
            &tool.licence.spdx_or_name,
            &tool.licence.notes,
        );
        assert!(!verdict.is_allowed());
    }

    #[test]
    fn gate_allows_a_permitted_seed_entry() {
        let catalog = load_seed().expect("seed");
        let recipe = resolve(&catalog, "node", "24.19.0", PLATFORM_WINDOWS_X64).expect("resolve");
        assert!(gate(&recipe).is_allowed(), "{recipe:?}");
    }

    #[test]
    fn public_api_covers_the_three_questions() {
        // 这个测试的用途是"如果有人删掉了某个 re-export，编译就失败"。
        let catalog: Catalog = load_seed().expect("seed");
        let _: Vec<&str> = catalog.tools.iter().map(|t| t.id.as_str()).collect();
        let _: Redistribution = catalog.tools[0].licence.redistribution;
        let _ = installable_versions(&catalog, "node", PLATFORM_WINDOWS_X64);
        let _ = SUPPORTED_SCHEMA_VERSION;
    }
}
