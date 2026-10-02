//! tuoen 的引擎。
//!
//! **分层规则（不得违反）**：本 crate 是**跨平台**的业务逻辑，**绝不直接调用 Win32**。
//! 一切平台相关能力（注册表、reparse point、环境块、进程）都通过 [`tuoen_platform`]
//! 提供的适配器进入，且适配器必须可注入 —— 这是测试能在不碰开发者真机的前提下运行的前提。
//!
//! 见 `docs/specs/L0-install-engine.md`（模块划分）与 `docs/specs/L1-dev-state.md`（测试 seam）。

pub mod capture;
pub mod detect;
pub mod list;
pub mod shim;

pub use capture::{
    CaptureBundle, CaptureError, CaptureOptions, Existence, SCHEMA_VERSION, Section, SkipEntry,
    SkippedFile, TargetExistence, capture, render, without_timestamp, write_bundle,
};
pub use detect::{Confidence, DetectedTool, DetectionSource, DetectionSummary};
pub use list::{ListResult, ToolRecord, ToolSource};
pub use shim::{ShimCommand, command_names, shim_commands, tool_for_command};
