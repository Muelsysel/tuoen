//! tuoen 自己的安装记录 —— 七层置信度里 `managed` 层的来源。
//!
//! **它只定义读接口的数据形状，不定义文件格式。**
//! 那是有意的：L0 的 `docs/specs/L0-install-engine.md` 才是规定"记录落在哪、长什么样"
//! 的地方，在这里发明一个（然后 L0 用另一个）会制造"两个真相"。
//! 现实中的结果就是决策 48 定下的存储布局，而**真实实现**见
//! `crates/cli/src/managed.rs` 的 `StoreManagedStore`（依赖方向的原因见下）。
//!
//! **为什么这一层非存在不可**：`managed` 是与另外六层并列的**判据**，
//! 判据必须可测。它在本票就有固定装置用例
//! （`fixtures/detect/confidence-managed.toml`），而 trait 让真实实现与固定装置
//! 可以互换 —— 检测引擎与七层判据一行都不用动。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 一条 tuoen 自己管理的安装记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedTool {
    /// 逻辑工具名（小写）。
    pub name: String,
    /// 已安装的版本。
    pub version: Option<String>,
    /// 安装根目录。
    pub path: String,
}

/// 读 tuoen 自己的安装记录。
pub trait ManagedStore {
    /// 全部记录。**读不到时返回空表而不是报错** ——
    /// "还没装过任何东西"与"记录读不了"在检测这一层的处理相同：
    /// 这一层没有条目，其它五层照常工作。
    fn installed(&self) -> Vec<ManagedTool>;
}

/// **空实现**：一个什么都不报的 `ManagedStore`。
///
/// ## 它现在为什么还是空的（而不是"还没做完"）
///
/// L0 已经落地了，但**真实实现没有放在这里**，而是放在 `tuoen-store` 的
/// 调用方（`crates/cli/src/managed.rs` 的 `StoreManagedStore`）。原因是依赖方向：
/// 存储布局（`<tool>/versions/<version>`、`<version>.json`、`current` 联接）
/// 的知识属于 `tuoen-store`，而 `tuoen-platform` **不能**依赖它 ——
/// 那会成环（`store` 依赖 `platform`）。
///
/// 把布局在这里再实现一遍是**能编译的**，但会让同一份布局同时存在于两个 crate，
/// 而它们的偏差只有在"`tuoen detect` 的结果与 `tuoen list` 不一致"时才暴露 ——
/// 那是最难归因的一类 bug。所以这里明确留空。
///
/// 它仍然有真实用途：**固定装置与"故意什么都不报"的测试**。
#[derive(Debug, Default, Clone)]
pub struct RealManagedStore {
    /// 保留的定位字段（决策 33 的命名一致性）。
    ///
    /// **注意**：`tuoen` 真实的存储根是 `%LOCALAPPDATA%\tuoen\store`
    /// （决策 48）。这里是 `%APPDATA%\tuoen`，是 L0 之前定下的旧位置，
    /// **不再是真实位置** —— 留着只因为它是这个空实现的构造参数。
    pub root: PathBuf,
}

impl RealManagedStore {
    /// 用给定的根目录构造（这个根目录**不会被读**，见本类型的文档）。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl ManagedStore for RealManagedStore {
    fn installed(&self) -> Vec<ManagedTool> {
        // **有意为空**：真实实现在 `tuoen-store` 的调用方。
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_empty_implementation_stays_empty_and_that_is_deliberate() {
        // 它**故意**恒为空：真实实现住在 `tuoen-store` 的调用方（见类型文档）。
        // 如果哪天有人"顺手把它填上"，存储布局就会有两个真相 —— 这条测试拦的就是那一步。
        let store = RealManagedStore::new(r"C:\Users\example\AppData\Roaming\tuoen");
        assert!(
            store.installed().is_empty(),
            "这个空实现必须恒为空：真实实现是 crates/cli/src/managed.rs 的 StoreManagedStore"
        );
        assert_eq!(
            store.root,
            PathBuf::from(r"C:\Users\example\AppData\Roaming\tuoen")
        );
    }
}
