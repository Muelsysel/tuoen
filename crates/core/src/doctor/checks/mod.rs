//! 23 条检查，按家族分四个文件。
//!
//! # 每个家族的形状都一样
//!
//! ```text
//! pub fn run(facts: &MachineFacts) -> Vec<Finding>
//! ```
//!
//! 纯函数：不 spawn、不读注册表、不写任何东西。**事实在 [`super::facts`] 里就已经
//! 采完了** —— 检查项只做判断。这条分界买到的是"每条检查的正例与反例都是一次结构体
//! 构造"，而票据明确要求每个正例都有反例。
//!
//! # 严重度怎么分（票据给的原则）
//!
//! - 会导致**静默失效**的是 `error`：切了 Node 版本，全局装的包悄悄不见了
//!   （`tool.global-prefix-inside-version-dir`）；`java` 与 `javac` 来自两个不同的
//!   安装，而用户以为是一个（`tool.multiple-active`）。
//! - 会导致**突然全体失效**的是 `error`：`PATH` 逼近 8191 之后 `cmd.exe` 会
//!   整条忽略它（`path.length-budget`）。
//! - 只是噪声的是 `info`：含空格的条目、reparse 条目、提权状态。
//!
//! # 一条检查报 0 条时，读的人要知道为什么
//!
//! 每条检查旁边都写着它的**反例**（健康的机器上它为什么不该响）。`doctor` 最容易
//! 犯的错不是报错，而是在什么都没看的情况下报 0 条 —— 所以事实里带着规模
//! （[`super::FactsSummary`]），而检查项该报的"分母"会写进 `evidence`。

pub mod env;
pub mod path;
pub mod system;
pub mod tool;
