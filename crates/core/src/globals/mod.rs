//! 「tuoen 管着哪些全局包」这件事的模块边界（ticket #23）。
//!
//! 三块：
//!
//! * [`root`] —— 根在**哪里**、重定向变量叫什么。**形状冻结**：后面的票
//!   （`globals add/remove`、安装重定向）只准调它，不许自己拼路径或变量名。
//! * [`bins`] —— 一个包**提供了哪些命令**（`binNames` 的唯一来源）。
//! * [`listing`] —— `tuoen globals list` 的数据面（两个来源的根 + 两个来源的包）。
//! * [`install`] —— 把包装进我们自己的根：分类、命令形状、staging + 翻转、逐包结果
//!   （票据 #24）。它是 `restore --only globals --apply` 的引擎，**纯函数部分**
//!   （`globals_wanted`）与**会起进程的部分**（`probe_npm_cache` / `install_globals`）
//!   分得很清楚 —— 计划与执行读的是同一个分类函数。
//!
//! # 枚举不在这里
//!
//! "工具自己说它的全局清单是什么"这件事仍然只有一处实现：
//! [`crate::capture::collect::globals`]。`capture` 与 `globals list` 都调它 ——
//! 两份枚举迟早会漂移，而漂移的表现是"`capture` 说 7 个、`globals list` 说 6 个"。
//! 这个模块只负责**名字与位置**（根、变量名、命令名）与**安装**。

mod bins;
mod install;
mod listing;
mod root;

pub use install::{
    FAILURE_INSTALL_FAILED, FAILURE_NEEDS_NETWORK, FAILURE_NOT_CACHED, FAILURE_VERSION_NOT_FOUND,
    GLOBAL_INSTALL_TIMEOUT, GlobalConflict, GlobalInstall, GlobalPrefixMoved, GlobalPresent,
    GlobalUnsupported, GlobalsFailure, GlobalsInstallReport, GlobalsWanted, PackageOutcome,
    PackageVersion, RESULT_ALREADY_PRESENT, RESULT_INSTALL_FAILED, RESULT_INSTALLED,
    RESULT_NOT_CACHED, RESULT_SKIPPED_SHADOWED, RESULT_UNSUPPORTED, RESULT_VERSION_CONFLICT,
    RESULT_VERSION_NOT_FOUND, STAGING_PREFIX, cache_key, globals_wanted, install_globals,
    probe_npm_cache,
};
pub use listing::{
    GlobalsListing, GlobalsNote, GlobalsNoteCode, GlobalsPackageRow, GlobalsRootRow, GlobalsSource,
    list_globals,
};
pub use root::{
    GlobalsRoot, GlobalsRootError, GlobalsTool, NPM_PREFIX_VAR, PIP_USER_VALUE, PIP_USER_VAR,
    PYTHONUSERBASE_VAR, UNKNOWN_VERSION,
};
// crate 内部还要用它：`capture` 的 `globals` 那一节按同一套规则算根（决策 187 的
// 那一条 —— `LOCALAPPDATA` 在**进程**环境里，注册表里根本没有它）。
pub(crate) use listing::root_from_context;
