//! 环境块的抽象。
//!
//! **为什么这需要抽象**：环境块在 `CreateProcess` 时复制 —— 只有 Explorer 及其之后新起的
//! 子进程能拿到 `PATH` 变更，已在运行的 cmd / PowerShell / VS Code / IDE / 服务 / 计划任务
//! 都拿不到，**我们自己的进程也拿不到**（除非显式打补丁）。
//! "请重启终端"是平台限制，不是工具不友好。
//!
//! 因此读取环境有两个不同的来源，必须区分：
//! - **注册表**（`HKCU\Environment` / `HKLM\...\Session Manager\Environment`）：持久值，`DoNotExpandEnvironmentNames` 可拿原始值
//! - **进程环境块**：当前进程实际看到的（含进程注入项，本机实测有 77 字符的 PowerShell MSIX 别名**不在任何注册表里**）
//!
//! 测试**绝不能读真实的注册表或真实的环境块** —— 所以这里全部是 trait。

use serde::{Deserialize, Serialize};

/// 一个环境变量的来源层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EnvScope {
    /// `HKCU\Environment` —— 用户级，不需要提权。
    User,
    /// `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` —— 机器级，需要提权才能写。
    Machine,
    /// 只存在于当前进程的环境块里（进程注入项）。**不在任何注册表里**，因此只读注册表的实现会漏掉它。
    ProcessOnly,
}

impl EnvScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Machine => "machine",
            Self::ProcessOnly => "process-only",
        }
    }
}

/// 注册表值类型。
///
/// **写回时必须保留原始类型**（除非值含 `%`）：这是"值含 `%` 才用 `REG_EXPAND_SZ`，
/// 否则保留原类型，再否则 `REG_SZ`"这条规则的落点。本机实测：机器级 `Path` 标了
/// `REG_EXPAND_SZ` 却**零变量**，用户级 `Path` 是 `REG_SZ`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegType {
    /// `REG_SZ` —— 不做展开。
    Sz,
    /// `REG_EXPAND_SZ` —— 读出来时会被展开。
    ExpandSz,
}

/// 一个环境变量的完整记录：原始值与展开值都要留。
///
/// **为什么两个都要**：`setx` 事故的典型残留就是"值里字面含有 `%VAR%` 但类型不是
/// `REG_EXPAND_SZ`"（或反之）。只留一个值就检测不到它。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVar {
    /// 变量名。**可以含空格**（本机 `IntelliJ IDEA` 就是这样，合法但很多工具会静默跳过）。
    pub name: String,
    /// 注册表里的原始值（未展开）。
    pub value_raw: String,
    /// 展开后的值。展开失败时等于 `value_raw`。
    pub value_expanded: String,
    pub scope: EnvScope,
    pub reg_type: RegType,
    /// 展开后的值是否指向一个存在的目标。**注意**：这在原机器上通过，不代表换机后也通过。
    pub target_exists: bool,
}

/// 读取环境变量的抽象。
///
/// 真实实现读注册表；测试实现读内存里的固定装置。
pub trait EnvBlock {
    /// 按 scope 列出全部变量。
    ///
    /// **不展开 `%VAR%`** —— 展开由调用方按 `reg_type` 决定，因为读的时候展开会破坏往返。
    fn list(&self, scope: EnvScope) -> Vec<EnvVar>;

    /// 取单个变量。不存在时返回 `None`。
    fn get(&self, scope: EnvScope, name: &str) -> Option<EnvVar>;
}

/// 读取**当前进程**环境块的抽象。
///
/// 与 [`EnvBlock`] 分开，因为进程环境块含有注册表里没有的条目（本机实测：
/// PowerShell 的 MSIX 别名注入了 77 字符），而捕获时两者都要，且必须标注来源。
pub trait ProcessEnv {
    /// 当前进程看到的完整环境（名字 → 值，已展开）。
    fn vars(&self) -> Vec<(String, String)>;

    /// 当前进程看到的 `PATH`，**按顺序**。
    fn path_entries(&self) -> Vec<String>;
}

/// 一个不读任何真实状态的 `ProcessEnv` 实现，供测试与"只想看注入项"的场景使用。
#[derive(Debug, Clone, Default)]
pub struct InMemoryEnv {
    vars: Vec<(String, String)>,
}

impl InMemoryEnv {
    #[must_use]
    pub fn new(vars: Vec<(String, String)>) -> Self {
        Self { vars }
    }

    /// 从一个 `PATH` 字符串构造，便于测试。
    #[must_use]
    pub fn with_path(path: &str) -> Self {
        Self::new(vec![("Path".to_owned(), path.to_owned())])
    }
}

impl ProcessEnv for InMemoryEnv {
    fn vars(&self) -> Vec<(String, String)> {
        self.vars.clone()
    }

    fn path_entries(&self) -> Vec<String> {
        self.vars
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("path"))
            .map(|(_, v)| v.split(';').map(ToOwned::to_owned).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_labels_are_the_public_contract() {
        assert_eq!(EnvScope::User.as_str(), "user");
        assert_eq!(EnvScope::Machine.as_str(), "machine");
        assert_eq!(EnvScope::ProcessOnly.as_str(), "process-only");
    }

    #[test]
    fn in_memory_path_splits_on_semicolon_and_keeps_empty_entries() {
        // 空条目必须保留：本机 HKLM Path 里真的有 `;;`，丢掉它就等于丢掉一个发现。
        let env = InMemoryEnv::with_path(r"C:\Windows;;C:\Tools");
        assert_eq!(
            env.path_entries(),
            vec![
                r"C:\Windows".to_owned(),
                String::new(),
                r"C:\Tools".to_owned()
            ]
        );
    }

    #[test]
    fn in_memory_path_is_case_insensitive_on_the_variable_name() {
        let env = InMemoryEnv::new(vec![("PATH".to_owned(), r"C:\A;C:\B".to_owned())]);
        assert_eq!(env.path_entries().len(), 2);
    }

    #[test]
    fn missing_path_yields_no_entries() {
        let env = InMemoryEnv::new(vec![("HOME".to_owned(), r"C:\Users\x".to_owned())]);
        assert!(env.path_entries().is_empty());
    }
}
