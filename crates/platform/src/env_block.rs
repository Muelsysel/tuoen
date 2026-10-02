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

// ─────────────────────────────────────────────────────────────────────────────
// 真实实现
// ─────────────────────────────────────────────────────────────────────────────

/// 用户级环境变量所在的子键。
///
/// **是 `Environment`，不带任何前缀。** 用户级环境变量与机器级的一样，
/// **不在 `SOFTWARE` 下面**：真实位置就是 `HKCU\Environment`。
///
/// 这条曾经写成 `"Environment"` 却被 `RegHive::Hkcu` 自动拼成
/// `HKCU\SOFTWARE\Environment` —— **那个键不存在**，于是用户级环境变量
/// **静默返回空表**。后来改成 `"SOFTWARE\Environment"` 是**错的修法**：
/// 它让前缀看起来对了，实际读的仍然是一个不存在的键。
///
/// 正确的修法在 `RegHive::resolve` 那一侧：`Environment` 被登记为
/// **单段但是绝对**的子键名（见 `ABSOLUTE_EXACT`）。
///
/// 症状是"没有报错、只是少了一半数据"：`NVM_HOME` 在 `HKCU\Environment` 与
/// `HKLM\...\Session Manager\Environment` 两处都有，而报告里只出现 `（machine）`。
///
/// 见 `crates/platform/src/env_block.rs` 的 `user_env_subkey_is_absolute` 用例。
pub const USER_ENV_SUBKEY: &str = "Environment";

/// 机器级环境变量所在的子键。
///
/// **注意它不在 `SOFTWARE` 下面** —— 这是 [`crate::RegHive::resolve`] 必须支持
/// 绝对路径的原因。这条路径是 Windows 上最容易写错的一条注册表路径之一。
pub const MACHINE_ENV_SUBKEY: &str =
    r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

/// 真实实现：从注册表读持久环境变量。
///
/// **两个泛型而不是两个 `&dyn`**：这样测试可以直接塞进 `FakeRegistry` + `FakeFileSystem`，
/// 从而在不碰真实注册表、真实磁盘的前提下测"读出来的 `EnvVar` 形状对不对"。
#[derive(Debug, Clone, Copy)]
pub struct RealEnvBlock<R: crate::Registry, F: crate::FileSystem> {
    registry: R,
    fs: F,
}

impl<R: crate::Registry, F: crate::FileSystem> RealEnvBlock<R, F> {
    #[must_use]
    pub const fn new(registry: R, fs: F) -> Self {
        Self { registry, fs }
    }

    fn read(&self, scope: EnvScope, subkey: &str) -> Vec<EnvVar> {
        let hive = match scope {
            EnvScope::User => crate::RegHive::Hkcu,
            EnvScope::Machine => crate::RegHive::Hklm,
            // 进程环境块不属于注册表；调用方该用 `RealProcessEnv`。
            EnvScope::ProcessOnly => return Vec::new(),
        };
        let Ok(values) = self.registry.values(hive, subkey) else {
            // 键不存在 = 这个 scope 没有变量，不是失败。
            return Vec::new();
        };

        // 展开 `%VAR%` 要用的名字表。用**已读到的变量本身**：
        // Windows 的行为就是这样（同一个键里的变量互相引用是常见写法，
        // 例如本机的 `NVM_SYMLINK` 与 `NVM_HOME`）。
        let lookup: Vec<(String, String)> = values
            .iter()
            .filter_map(|(name, value)| value.as_str().map(|text| (name.clone(), text.to_owned())))
            .collect();

        values
            .into_iter()
            .filter_map(|(name, value)| {
                let text = value.as_str()?.to_owned();
                let reg_type = match value {
                    crate::RegValue::ExpandSz(_) => RegType::ExpandSz,
                    // `REG_MULTI_SZ` 的 `Path` 在真实机器上存在（历史上是常见写法）。
                    // 当成 `REG_SZ` 处理并保留原样，比丢掉它好。
                    _ => RegType::Sz,
                };
                let expanded = crate::expand_vars(&text, &lookup);
                // **真的去看一眼目标在不在。** 这条检测有真实价值：本机的
                // `HALCONROOT` 指向一个不存在的目录，而换账号名之后
                // 11 条硬编码用户名的路径会静默失效。
                let target_exists = expanded
                    .split(';')
                    .map(str::trim)
                    .find(|part| !part.is_empty())
                    .is_some_and(|first| self.fs.inspect(std::path::Path::new(first)).exists);
                Some(EnvVar {
                    name,
                    value_raw: text,
                    value_expanded: expanded,
                    scope,
                    reg_type,
                    target_exists,
                })
            })
            .collect()
    }
}

impl<R: crate::Registry, F: crate::FileSystem> EnvBlock for RealEnvBlock<R, F> {
    fn list(&self, scope: EnvScope) -> Vec<EnvVar> {
        match scope {
            EnvScope::User => self.read(scope, USER_ENV_SUBKEY),
            EnvScope::Machine => self.read(scope, MACHINE_ENV_SUBKEY),
            EnvScope::ProcessOnly => Vec::new(),
        }
    }

    fn get(&self, scope: EnvScope, name: &str) -> Option<EnvVar> {
        self.list(scope)
            .into_iter()
            .find(|var| var.name.eq_ignore_ascii_case(name))
    }
}

/// 真实实现：读**当前进程**的环境块。
///
/// **它与注册表读出来的值会不一样，而那个不一样本身是信息**：本机实测有 77 字符的
/// PowerShell MSIX 别名只存在于进程环境块里，注册表里一条都没有。
#[derive(Debug, Clone, Copy, Default)]
pub struct RealProcessEnv;

impl RealProcessEnv {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ProcessEnv for RealProcessEnv {
    fn vars(&self) -> Vec<(String, String)> {
        std::env::vars().collect()
    }

    fn path_entries(&self) -> Vec<String> {
        std::env::var("Path")
            .or_else(|_| std::env::var("PATH"))
            .map(|value| value.split(';').map(ToOwned::to_owned).collect())
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
    fn user_env_subkey_is_absolute_so_it_does_not_get_the_software_prefix_twice() {
        // **这条用例守着一个真机上活过的 bug。**
        //
        // `RegHive::Hkcu.resolve()` 会给不带绝对前缀的子键自动加 `SOFTWARE\`。
        // 而**用户级环境变量在 `HKCU\Environment`，不在 `HKCU\SOFTWARE\Environment`**
        // —— 那个键不存在，于是用户级环境变量**静默返回空表**。
        //
        // 症状是"没有报错、只是少了一半数据"：`NVM_HOME` 在 `HKCU\Environment`
        // 与 `HKLM\...\Session Manager\Environment` 两处都有，而报告里只出现 `（machine）`。
        let (_, resolved) = crate::RegHive::Hkcu.resolve(USER_ENV_SUBKEY);
        assert_eq!(
            resolved, r"Environment",
            "用户级环境变量在 HKCU\\Environment —— 它不在 SOFTWARE 下面，不能被拼上那个前缀"
        );

        let (_, machine) = crate::RegHive::Hklm.resolve(MACHINE_ENV_SUBKEY);
        assert_eq!(
            machine, r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            "机器级环境变量不在 SOFTWARE 下面"
        );
        // 两者必须落在**不同**的注册表路径上 —— 否则读两遍同一个键，
        // "双重管理"这类跨 scope 的发现会整个消失。
        assert_ne!(resolved, machine);
    }

    #[test]
    fn only_the_exact_name_environment_is_treated_as_absolute() {
        // `strip_absolute_prefix` 对单段名字必须**精确匹配**：
        // 用 `starts_with` 会让 `EnvironmentFoo` 也被当成绝对路径，
        // 于是它绕过 `SOFTWARE\` 前缀去读一个不存在的键 —— 又是一次静默返空。
        let (_, real) = crate::RegHive::Hkcu.resolve("Environment");
        assert_eq!(real, r"Environment");

        let (_, fake) = crate::RegHive::Hkcu.resolve("EnvironmentFoo");
        assert_eq!(
            fake, r"SOFTWARE\EnvironmentFoo",
            "只有 Environment 这个名字本身是绝对的，加了后缀就必须走相对解析"
        );

        // 大小写不敏感（注册表就是不敏感的）。
        let (_, upper) = crate::RegHive::Hkcu.resolve("ENVIRONMENT");
        assert_eq!(upper, r"ENVIRONMENT");

        // 但带分隔符的绝对前缀仍然只按前缀判定。
        let (_, sub) = crate::RegHive::Hkcu.resolve(r"Environment\Sub");
        assert_eq!(sub, r"Environment\Sub", "带分隔符时按前缀判定");
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
