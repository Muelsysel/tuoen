//! 检测引擎的**注入上下文**。
//!
//! **这个类型存在的唯一理由**：`docs/specs/L1-dev-state.md` 的硬性测试约束要求
//! "测试不得读取真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH` / 用户真实安装目录"。
//! 检测引擎只依赖 trait，所以测试可以塞进固定装置，而真机运行塞进真实实现。
//!
//! 每个字段都对应一个**只读**能力。这里没有写方法 —— "这一票只读"是类型事实。

use std::path::PathBuf;

use tuoen_platform::{
    EnvBlock, EnvScope, FileSystem, ManagedStore, ProcessEnv, ProcessRunner, Registry,
};

use crate::detect::spec::ToolSpec;

/// `PATH` 里一条条目的来源层级。
///
/// **必须区分 `machine` 与 `user`**：进程 `PATH` 的顺序是"机器级在前、用户级在后"，
/// 所以用户级条目**永远输掉名字冲突** —— 这是本项目必须用 shim 而不是调 `PATH` 顺序的原因
/// （ADR-0002）。检测出"谁遮蔽了谁"依赖这个字段。
///
/// `process-only` 是必需的第三档：本机实测有 77 字符的 PowerShell MSIX 别名
/// **不在任何注册表里**，只存在于当前进程的环境块。只读注册表的实现会漏掉它。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PathScope {
    /// 机器级（`HKLM\...\Session Manager\Environment` 的 `Path`）。**排在前面。**
    Machine,
    /// 用户级（`HKCU\Environment` 的 `Path`）。
    User,
    /// 只存在于当前进程环境块（进程注入项，不在任何注册表里）。
    ProcessOnly,
}

impl PathScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Machine => "machine",
            Self::User => "user",
            Self::ProcessOnly => "process-only",
        }
    }
}

/// `PATH` 上的一条条目，带来源层级与它在进程 `PATH` 里的位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathEntry {
    /// 原样的条目文本（可能含 `%VAR%`，也可能含尾部反斜杠）。
    pub raw: String,
    /// 来源层级。
    pub scope: PathScope,
    /// 在进程 `PATH` 里的序号（0 起）。**顺序即优先级。**
    pub index: usize,
}

/// 一个目录候选（文件系统扫描用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanRoot {
    /// 要扫的根目录。
    pub path: PathBuf,
    /// 人类可读的"为什么扫这里"（出现在 `evidence` 里，所以要说清楚）。
    pub why: &'static str,
}

/// 检测引擎的全部注入依赖。
pub struct DetectContext<'a> {
    /// 文件系统事实（含 reparse tag）。
    pub fs: &'a dyn FileSystem,
    /// 注册表（三个 hive 的 ARP、App Paths）。
    pub registry: &'a dyn Registry,
    /// 持久环境变量（用户级与机器级）。
    pub env: &'a dyn EnvBlock,
    /// 当前进程环境块（含注册表里没有的注入项）。
    pub process_env: &'a dyn ProcessEnv,
    /// 跑 `<tool> --version` 这类探测。
    pub runner: &'a dyn ProcessRunner,
    /// tuoen 自己的安装记录（L0 落地前恒为空）。
    pub managed: &'a dyn ManagedStore,
    /// 版本探测的超时。**必需**：一个挂住的工具不能挂住整个 `detect`。
    pub probe_timeout: std::time::Duration,
    /// 要不要跑版本探测。
    ///
    /// 关掉它有两个正当理由：① `doctor` 只关心结构不关心版本；
    /// ② 测试里想断言"发现但版本未知"这一条路径。
    pub probe_versions: bool,
    /// 文件系统扫描的根目录。
    pub scan_roots: Vec<ScanRoot>,
}

impl DetectContext<'_> {
    /// 读一个环境变量（先用户级、再机器级）。
    ///
    /// **顺序有语义**：用户级覆盖机器级是 Windows 的实际行为
    /// （进程环境块由两者合并，用户级在后）。
    #[must_use]
    pub fn env_var(&self, name: &str) -> Option<String> {
        for scope in [EnvScope::User, EnvScope::Machine] {
            if let Some(var) = self.env.get(scope, name) {
                return Some(var.value_expanded);
            }
        }
        None
    }

    /// 进程环境块里的一个变量。
    #[must_use]
    pub fn process_var(&self, name: &str) -> Option<String> {
        self.process_env
            .vars()
            .into_iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    /// 当前进程看到的全部变量（用于展开 `%VAR%`）。
    #[must_use]
    pub fn process_vars(&self) -> Vec<(String, String)> {
        self.process_env.vars()
    }

    /// 按规格探测一个工具的版本。
    ///
    /// **同时读两路**（由 `VersionStream` 决定读哪一路），因为本机实测
    /// `git --version` / `java -version` 走 stderr，而 `node -v` 走 stdout。
    /// 超时或启动失败时返回 `None` —— 上层记"发现但版本未知"，**不编一个版本**。
    #[must_use]
    pub fn probe_version(&self, spec: &ToolSpec, executable: &std::path::Path) -> Option<String> {
        use crate::detect::spec::VersionStream;

        if !self.probe_versions {
            return None;
        }
        let outcome = self
            .runner
            .run(executable, spec.version_args, self.probe_timeout);

        // 超时/启动失败：读到的可能是半截输出，**不用它** ——
        // 半个版本号比没有版本号更危险。
        if outcome.timed_out || !outcome.spawned {
            return None;
        }

        let text = match spec.version_stream {
            VersionStream::Stdout => outcome.stdout.clone(),
            VersionStream::Stderr => outcome.stderr.clone(),
            VersionStream::Both => outcome.combined(),
        };
        crate::detect::spec::extract_version(spec, &text)
    }
}

impl std::fmt::Debug for DetectContext<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DetectContext")
            .field("probe_timeout", &self.probe_timeout)
            .field("probe_versions", &self.probe_versions)
            .field("scan_roots", &self.scan_roots)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_scope_labels_are_the_public_contract() {
        assert_eq!(PathScope::Machine.as_str(), "machine");
        assert_eq!(PathScope::User.as_str(), "user");
        assert_eq!(PathScope::ProcessOnly.as_str(), "process-only");
    }

    #[test]
    fn machine_sorts_before_user_which_is_why_shims_are_needed() {
        // 顺序不是巧合：它编码"机器级条目在进程 PATH 里排在用户级之前"这条平台事实。
        assert!(PathScope::Machine < PathScope::User);
        assert!(PathScope::User < PathScope::ProcessOnly);
    }
}
