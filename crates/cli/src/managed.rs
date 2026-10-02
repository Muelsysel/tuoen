//! `managed` 层的**真实**实现：从 tuoen 自己的存储里读。
//!
//! ## 为什么这个适配器不在 `tuoen-platform` 里
//!
//! `RealManagedStore` 是 `tuoen-platform` 里的"空实现"（L0 之前的诚实占位），
//! 而它留了一句"L0 落地时只需要换掉一个实现"。**落地时那个实现放不进去** ——
//! 存储布局的知识（`<tool>/versions/<version>`、`<version>.json`、`current`）
//! 属于 `tuoen-store`，而 `tuoen-platform` **不能**依赖它（依赖方向是
//! `platform` ← `store`，反向会成环）。
//!
//! 在这里放一个有两条理由：
//!
//! 1. **布局只有一个真相。** 如果让 `platform` 自己去走一遍目录，
//!    存储布局就同时存在于两个 crate 里，而它们的偏差只有在"检测结果与
//!    `tuoen list` 不一致"时才会暴露 —— 那种 bug 极难归因。
//! 2. **检测的判据必须能被固定装置替换。** `ManagedStore` 是 trait，
//!    这里只是它诸多实现中的一个；`fixtures/detect/confidence-managed.toml`
//!    仍然用假实现。
//!
//! ## 一个重要后果：记录在而载荷不在
//!
//! 记录（`<version>.json`）与载荷（`<version>/`）是两个东西，而**目录是事实来源**
//! （决策 48）。所以这里**只报目录真的在的版本**：
//!
//! - 记录被删、目录还在 → 照常报（只是说不出它从哪来）
//! - 目录被删、记录还在 → **不报**。报它会让 `detect` 声称一个不存在的安装是
//!   "由 tuoen 自己安装"（最高置信度），而幽灵条目正是本工具要消灭的东西。
//!
//! ## 一个诚实的取舍：`version` 报的是**生效版本**
//!
//! 一个工具可以同时装着 3 个版本。`detect` 的表格一个工具一行（决策 37），
//! 所以这里报生效的那个；**没有生效版本时报存储里最新的那个**并让路径指过去，
//! 这样"装了但没激活"不会在检测结果里变成"没装"。

use tuoen_platform::{ManagedStore, ManagedTool};
use tuoen_store::Store;

/// 从 tuoen 的存储里读安装记录。
#[derive(Debug, Clone)]
pub struct StoreManagedStore {
    store: Store,
}

impl StoreManagedStore {
    #[must_use]
    pub fn at_default_location() -> Self {
        Self::new(Store::at_default_location())
    }

    #[must_use]
    pub fn new(store: Store) -> Self {
        Self { store }
    }
}

impl ManagedStore for StoreManagedStore {
    fn installed(&self) -> Vec<ManagedTool> {
        tuoen_store::installed_tools(&self.store)
            .into_iter()
            .filter_map(|tool| {
                let versions = tuoen_store::installed_versions(&self.store, &tool);
                // `installed_versions` 已经跳过 `.staging-*`、非目录条目与不安全的名字，
                // 并且**只报目录真的在的版本** —— 这里不再重复判一遍，
                // 否则就会有两套判据（正是本文件开头要避免的事）。
                let active = tuoen_store::active_version(&self.store, &tool);
                let (version, path) = match (&active, versions.first()) {
                    (Some(active), _) => {
                        (Some(active.clone()), self.store.version_dir(&tool, active))
                    }
                    // 装了但没激活：报最新的那个，并且**路径指向它** ——
                    // 报一个 `current` 会让检测结果里的路径变成一个不存在的链接。
                    (None, Some(latest)) => (
                        Some(latest.version.clone()),
                        self.store.version_dir(&tool, &latest.version),
                    ),
                    // 只有版本目录骨架、里面一个版本都没有。
                    // 这不算一个安装 —— 报它会让 `detect` 多出一条空条目。
                    (None, None) => return None,
                };
                Some(ManagedTool {
                    name: tool,
                    version,
                    path: path.display().to_string(),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::test_support::TempDir;

    #[test]
    fn an_empty_store_reports_nothing() {
        let dir = TempDir::new("managed-empty");
        let managed = StoreManagedStore::new(Store::new(dir.path()));
        assert!(managed.installed().is_empty());
    }

    #[test]
    fn a_version_directory_without_a_record_is_still_reported() {
        // **目录是事实来源**（决策 48）：用户手删了那份 JSON，安装仍然是真的。
        let dir = TempDir::new("managed-no-record");
        let store = Store::new(dir.path());
        std::fs::create_dir_all(store.version_dir("node", "24.19.0")).expect("造版本目录");

        let managed = StoreManagedStore::new(store).installed();
        assert_eq!(managed.len(), 1);
        assert_eq!(managed[0].name, "node");
        assert_eq!(managed[0].version.as_deref(), Some("24.19.0"));
    }

    #[test]
    fn a_tool_skeleton_without_versions_is_not_an_install() {
        // `versions/` 存在但是空的 —— 这不该在检测结果里变成"由 tuoen 安装"。
        let dir = TempDir::new("managed-skeleton");
        let store = Store::new(dir.path());
        std::fs::create_dir_all(store.versions_dir("node")).expect("造骨架");

        let managed = StoreManagedStore::new(store).installed();
        assert!(managed.is_empty(), "只有骨架不算安装：{managed:?}");
    }

    #[test]
    fn the_active_version_wins_over_the_newest_one() {
        // 同时装着两个版本时，报的必须是**生效**的那个 ——
        // 报"最新的"会让检测结果说"你在用 24.21.0"，而实际敲 `node` 跑的是 24.19.0。
        let dir = TempDir::new("managed-active-wins");
        let store = Store::new(dir.path());
        std::fs::create_dir_all(store.version_dir("node", "24.19.0")).expect("造 v1");
        std::fs::create_dir_all(store.version_dir("node", "24.21.0")).expect("造 v2");
        tuoen_store::activate(&store, "node", "24.19.0").expect("激活旧版本");

        let managed = StoreManagedStore::new(store).installed();
        assert_eq!(managed.len(), 1);
        assert_eq!(
            managed[0].version.as_deref(),
            Some("24.19.0"),
            "生效的是 24.19.0，不是更新的 24.21.0"
        );
    }

    #[test]
    fn without_an_active_version_the_newest_is_reported() {
        // 装了但没激活时**必须报出来**：报"没装"会让人以为要重装一遍。
        let dir = TempDir::new("managed-no-active");
        let store = Store::new(dir.path());
        std::fs::create_dir_all(store.version_dir("node", "24.19.0")).expect("造 v1");
        std::fs::create_dir_all(store.version_dir("node", "24.21.0")).expect("造 v2");

        let managed = StoreManagedStore::new(store).installed();
        assert_eq!(managed.len(), 1);
        assert_eq!(managed[0].version.as_deref(), Some("24.21.0"));
    }
}
