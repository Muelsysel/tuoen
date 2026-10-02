//! tuoen 的平台层。
//!
//! **存在理由**：把所有 Win32 调用收在这一个 crate 里，业务逻辑（`tuoen-core`）
//! 就永远不会出现 `#[cfg(windows)]`。这不是洁癖 —— 它是让业务逻辑能被测试的前提，
//! 因为测试**绝不能碰开发者的真实注册表、真实 `PATH` 或真实安装目录**。
//!
//! **可注入性是硬要求**：每个访问机器状态的能力都必须有"真实实现"与"测试实现"两个版本，
//! 且业务逻辑只依赖抽象。见 `docs/specs/L0-install-engine.md` 的模块划分一节。
//!
//! 本 crate 目前只定义抽象与"环境事实"的最小查询（ticket #2）。注册表读写、
//! reparse point 判定、进程与环境块操作分别在 ticket #8 与 #11 落地。

use serde::{Deserialize, Serialize};

pub mod env_block;

pub use env_block::{EnvBlock, ProcessEnv};

/// 当前进程所处的权限与平台状态。
///
/// **这些事实决定哪些还原步骤可行**，所以必须在 plan 里可见，而不是在执行时才失败。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostFacts {
    /// 当前进程是否已提权。决定机器级环境变量与 MSVC bootstrapper 能否直接做。
    pub elevated: bool,
    /// Developer Mode 是否开启。**决定 symlink 是否可用**：未提权且未开 Developer Mode 时
    /// `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`(0x2) 无效（本机实测）。
    pub developer_mode: bool,
    /// `LongPathsEnabled` 是否为 1。**注意**：即使为 1，仍要求程序清单含
    /// `<ws2:longPathAware>true</ws2:longPathAware>`，且**相对路径永远受 MAX_PATH 限制**。
    pub long_paths_enabled: bool,
    /// `cmd.exe` 的 `PATH` 悬崖（8191 字符）。超过后**整条 `PATH` 一次性全部失效**。
    pub path_cliff: usize,
}

/// 所有实现必须共用的悬崖常量。
///
/// **与 `setx` 的 1024 裁剪是两个不同的悬崖**：1024 是静默数据丢失，8191 是全体命令失效。
pub const PATH_CLIFF_CMD: usize = 8191;

/// `setx` 的裁剪上限。**仅用于诊断报告**，本项目**绝不调用 `setx`**。
pub const SETX_TRUNCATION: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cliffs_are_the_two_distinct_numbers_from_research() {
        // 把这两个数字钉在测试里，避免有人"顺手统一"它们。
        assert_eq!(PATH_CLIFF_CMD, 8191);
        assert_eq!(SETX_TRUNCATION, 1024);
        assert_ne!(PATH_CLIFF_CMD, SETX_TRUNCATION);
    }
}
