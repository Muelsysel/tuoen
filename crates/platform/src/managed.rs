//! tuoen 自己的安装记录 —— 七层置信度里 `managed` 层的来源。
//!
//! **现状：L0（安装引擎）还没落地，所以真实实现恒返回空表。**
//! 这不是占位：`docs/specs/L1-dev-state.md` 的判据表里 `managed` 一栏写的就是
//! "由 `tuoen` 自己安装（存在我们的安装记录）"，括号里那句"（L0 完成后才有）"
//! 指的是**记录的产生者**，不是这一层的存在性。
//!
//! **为什么现在就要把它做成 trait**：`managed` 是与另外五层并列的**判据**，
//! 判据必须可测。把它做成可注入的 seam 之后：
//! - 它在本票就有固定装置用例（`fixtures/detect/confidence-managed.toml`）；
//! - L0 落地时只需要换掉 [`RealManagedStore`] 一个实现，检测引擎与七层判据不动。
//!
//! **记录格式故意不定**：L0 的 `docs/specs/L0-install-engine.md` 没有规定安装记录的落盘
//! 位置与格式，而在这里发明一个（然后 L0 用另一个）会制造"两个真相"。
//! 所以本文件只定义**读接口的数据形状**，不定义文件格式。

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

/// 真实实现。**L0 落地前恒为空**。
#[derive(Debug, Default, Clone)]
pub struct RealManagedStore {
    /// `%APPDATA%\tuoen` —— L0 的安装记录将落在这里（决策 33 的命名一致性）。
    /// 现在只被读来定位，不做任何解释。
    pub root: PathBuf,
}

impl RealManagedStore {
    /// 默认位置：`%APPDATA%\tuoen`。
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
}

impl ManagedStore for RealManagedStore {
    fn installed(&self) -> Vec<ManagedTool> {
        // **有意为空**：见本文件头部注释。L0 落地后换掉这一个函数体即可。
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_real_store_is_empty_until_l0_lands_and_that_is_deliberate() {
        let store = RealManagedStore::new(r"C:\Users\example\AppData\Roaming\tuoen");
        assert!(
            store.installed().is_empty(),
            "L0 未落地时 managed 层必须为空 —— 编一条假记录会让真机输出不可信"
        );
        assert_eq!(
            store.root,
            PathBuf::from(r"C:\Users\example\AppData\Roaming\tuoen")
        );
    }
}
