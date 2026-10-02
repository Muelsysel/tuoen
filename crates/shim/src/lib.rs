//! tuoen 的 shim —— 一个**真 `.exe`** 转发器。
//!
//! **为什么必须是真 `.exe`**（而不是 `.cmd` / `.ps1` 边车）：
//! ① 用户级工具在 `PATH` 上永远排在机器级条目之后（本机实测：进程 `PATH` 首条为机器条目），
//! 只能靠 shim 抢名字；
//! ② `.cmd` shim 在 Node ≥18.20.2 / 20.12.2 / 21.7.3 之后**无法被 spawn**
//! （CVE-2024-27980，nodejs/node#52681）。
//!
//! **为什么必须独立成 crate**：shim 是每次敲 `node` 都要跑的程序。
//! 把 GUI 或下载层的依赖图拖进它的启动路径是不可接受的。
//! 实测参考：Scoop 的原生 shim 约 33–36ms，C# 版约 86ms。
//!
//! **本 crate 目前只有形状**（ticket #2）。参数转发、退出码透传、Ctrl-C 透传、
//! 以及启动开销实测在 ticket #7 落地。见 `docs/adr/0002-shims-not-path-order.md`。

use serde::{Deserialize, Serialize};

/// shim 的边车配置。
///
/// **目标路径在生成时写死**（决策 11）：因为用 junction 翻转，shim 指向稳定的
/// `.../current/<tool>.exe`，切版本根本不碰 shim —— 省掉每次调用的配置读取与解析。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShimSpec {
    /// shim 自己的名字（`node.exe` 的 `node`）。
    pub name: String,
    /// **写死的**目标可执行文件绝对路径。
    pub target: String,
    /// 透传给目标的固定前缀参数。
    #[serde(default)]
    pub args: Vec<String>,
    /// 目标的工作目录，`None` 表示继承调用者。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 生成这个 shim 的 tuoen 版本，便于诊断"shim 是旧版生成的"。
    pub generated_by: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_round_trips_and_target_is_absolute() {
        let spec = ShimSpec {
            name: "node".to_owned(),
            target: r"C:\Users\example\AppData\Local\tuoen\tools\node\current\node.exe".to_owned(),
            args: vec![],
            cwd: None,
            generated_by: "0.1.0".to_owned(),
        };
        let json = serde_json::to_string(&spec).expect("serialise");
        let back: ShimSpec = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, spec);
    }

    #[test]
    fn spec_without_optional_fields_still_deserialises() {
        // 边车文件是人工可编辑的：缺字段不能导致解析失败。
        let json = r#"{"name":"node","target":"C:\\x\\node.exe","generated_by":"0.1.0"}"#;
        let spec: ShimSpec = serde_json::from_str(json).expect("deserialise");
        assert!(spec.args.is_empty());
        assert!(spec.cwd.is_none());
    }
}
