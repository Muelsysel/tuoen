//! `restore` 的**测试夹具**（不是产品代码）。
//!
//! `plan` 的两侧都是内存里的 [`RestoreBundle`]，所以这一票的用例**根本不需要
//! 触碰真实机器**：给两份 bundle，拿一份计划。这个模块提供造那两份 bundle 的东西。
//!
//! * [`tools`] / [`env`] / [`wsl`] / [`skipped`] —— 造各个 section，字段填的是
//!   **稳定的假值**（用例不该依赖系统里真的有什么）；
//! * [`path`] —— 直接转发 `pathdiff` 的夹具（§1.17 已经把它写好了，抄一份
//!   等于让两边的"一行长什么样"漂移）；
//! * [`bundle`] —— 一个**链式**的 bundle 构造器，让"两侧同一形状"这件事一眼看得出来。
//!
//! `ToolRow.path` 与 `ToolRow.evidence` 刻意填成真机的形状（一个是路径、一个是中文散文）：
//! 计划**永远不该读它们**，而"不该读"这件事只能靠"填了也不出现"来证明。
//!
//! 这个模块**没有**"出厂二进制的后门开关"：它只在测试与别的 crate 的测试里被引用。

use tuoen_platform::{EnvScope, RegType};

use crate::capture::{
    EnvFile, EnvVarRow, Existence, PathFile, SkipEntry, SkippedFile, TargetExistence, ToolRow,
    ToolsFile, WslFile, WslRow,
};
use crate::restore::RestoreBundle;

/// 夹具的捕获时间。**固定值**：用例里没有任何东西应该依赖"现在几点"。
pub const CAPTURED_AT: &str = "2026-10-02T12:00:00Z";

/// 造一份 `path.toml`：每条给（作用域、原文、存在性）。**转发 `pathdiff` 的夹具**。
#[must_use]
pub fn path(rows: &[(EnvScope, &str, Existence)]) -> PathFile {
    crate::pathdiff::test_support::path_file(rows)
}

/// 造一条工具记录。
///
/// `path` 与 `evidence` 填成**真机的形状**（后者是中文散文）—— 计划不许碰它们两个。
#[must_use]
pub fn tool(
    name: &str,
    version: Option<&str>,
    manager: Option<&str>,
    reproducible: bool,
) -> ToolRow {
    ToolRow {
        name: name.to_owned(),
        version: version.map(str::to_owned),
        path: format!(r"C:\tools\{name}"),
        source: "path-resolution".to_owned(),
        confidence: if reproducible {
            "executable".to_owned()
        } else {
            "manager-owned".to_owned()
        },
        manager: manager.map(str::to_owned),
        evidence: format!("夹具：{name} 是这么被发现的"),
        reproducible,
    }
}

/// 造一份 `tools.toml`：每条给（名字、版本、管理器、可复现）。
#[must_use]
pub fn tools(rows: &[(&str, Option<&str>, Option<&str>, bool)]) -> ToolsFile {
    tools_file(
        rows.iter()
            .map(|(name, version, manager, reproducible)| {
                tool(name, *version, *manager, *reproducible)
            })
            .collect(),
    )
}

/// 用现成的行造一份 `tools.toml`（要改 `evidence` / `path` 的用例走这个）。
#[must_use]
pub fn tools_file(rows: Vec<ToolRow>) -> ToolsFile {
    ToolsFile {
        schema_version: crate::capture::SCHEMA_VERSION,
        captured_at: CAPTURED_AT.to_owned(),
        tool: rows,
    }
}

/// 造一份 `env.toml`：每条给（作用域、名字、值）。
///
/// `target` 恒为 `None` + `NotAPath`：值是不是一条路径与还原的判据无关，
/// 而填一个假的"它指向哪里"会让用例看起来在验一件没人验的事。
#[must_use]
pub fn env(rows: &[(EnvScope, &str, &str)]) -> EnvFile {
    EnvFile {
        schema_version: crate::capture::SCHEMA_VERSION,
        captured_at: CAPTURED_AT.to_owned(),
        var: rows
            .iter()
            .map(|(scope, name, value)| EnvVarRow {
                name: (*name).to_owned(),
                scope: *scope,
                value_raw: (*value).to_owned(),
                value_expanded: (*value).to_owned(),
                reg_type: RegType::Sz,
                target: None,
                target_exists: TargetExistence::NotAPath,
            })
            .collect(),
    }
}

/// 造一份 `wsl.toml`：每条给（发行版名、`BasePath`）。
#[must_use]
pub fn wsl(rows: &[(&str, &str)]) -> WslFile {
    WslFile {
        schema_version: crate::capture::SCHEMA_VERSION,
        captured_at: CAPTURED_AT.to_owned(),
        distribution: rows
            .iter()
            .enumerate()
            .map(|(index, (name, base_path))| WslRow {
                name: (*name).to_owned(),
                guid: format!("{{00000000-0000-0000-0000-{index:012}}}"),
                base_path: (*base_path).to_owned(),
                non_standard_path: true,
                wsl_version: Some(2),
                state: Some(1),
                vhdx_path: format!("{base_path}\\ext4.vhdx"),
                vhdx_exists: Existence::Yes,
                vhdx_bytes: Some(1_000_000),
            })
            .collect(),
    }
}

/// 造一份 `skipped.toml`：每条给（section、名字、作用域、kind）。
///
/// `reason` 是中文散文 —— 与真机一样，而计划**不该读它**。
#[must_use]
pub fn skipped(rows: &[(&str, &str, Option<&str>, &str)]) -> SkippedFile {
    let mut file = SkippedFile::new(CAPTURED_AT);
    for (section, name, scope, kind) in rows {
        file.push(SkipEntry {
            section: (*section).to_owned(),
            scope: scope.map(str::to_owned),
            name: (*name).to_owned(),
            kind: (*kind).to_owned(),
            reason: format!("夹具：{name} 因为 {kind} 被跳过"),
        });
    }
    file
}

/// 一个空的 bundle 构造器。
#[must_use]
pub fn bundle() -> BundleBuilder {
    BundleBuilder::default()
}

/// 链式造一份 [`RestoreBundle`]。
#[derive(Debug, Clone, Default)]
pub struct BundleBuilder {
    bundle: RestoreBundle,
}

impl BundleBuilder {
    /// **用户敲的那个路径**（决策 162）。目标侧用它，本机侧留空。
    #[must_use]
    pub fn source_of(mut self, dir: &str) -> Self {
        self.bundle.source = Some(dir.to_owned());
        self
    }

    /// 加一段 `tools.toml`。
    #[must_use]
    pub fn tools(mut self, file: ToolsFile) -> Self {
        self.bundle.tools = Some(file);
        self
    }

    /// 加一段 `path.toml`。
    #[must_use]
    pub fn path(mut self, file: PathFile) -> Self {
        self.bundle.path = Some(file);
        self
    }

    /// 加一段 `env.toml`。
    #[must_use]
    pub fn env(mut self, file: EnvFile) -> Self {
        self.bundle.env = Some(file);
        self
    }

    /// 加一段 `wsl.toml`。
    #[must_use]
    pub fn wsl(mut self, file: WslFile) -> Self {
        self.bundle.wsl = Some(file);
        self
    }

    /// 加一份 `skipped.toml`。
    #[must_use]
    pub fn skipped(mut self, file: SkippedFile) -> Self {
        self.bundle.skipped = Some(file);
        self
    }

    /// 完事。
    #[must_use]
    pub fn build(self) -> RestoreBundle {
        self.bundle
    }
}

/// 一个**自建自删**的临时目录，给 [`RestoreBundle::load`] 的用例用。
///
/// `plan` 是纯函数，不需要真文件；只有"从 `tuoen.d/` 读一份快照"这件事需要，
/// 而它**只碰 `%TEMP%` 下面自己建的那一层**（名字带进程 id 与自增序号，
/// 并发的用例不会互相踩；`Drop` 里整个删掉）。
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TempDir {
    path: std::path::PathBuf,
}

#[cfg(test)]
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
impl TempDir {
    /// 建一个临时目录。
    pub(crate) fn new(prefix: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "tuoen-restore-{prefix}-{}-{nanos}-{id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("建临时目录");
        Self { path }
    }

    /// 目录本身。
    pub(crate) fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// 写一个文件（连父目录一起）。
    pub(crate) fn write(&self, name: &str, text: &str) -> std::path::PathBuf {
        let path = self.path.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("建父目录");
        }
        std::fs::write(&path, text).expect("写文件");
        path
    }
}

#[cfg(test)]
impl Drop for TempDir {
    fn drop(&mut self) {
        // 删不掉就算了：测试的清理失败不该盖住测试真正的结论。
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fixture_rows_look_like_the_real_thing() {
        let file = tools(&[("node", Some("24.19.0"), Some("nvm4w"), false)]);
        let row = &file.tool[0];
        assert_eq!(row.name, "node");
        assert_eq!(row.manager.as_deref(), Some("nvm4w"));
        assert!(!row.reproducible);
        assert!(
            row.evidence.contains('：'),
            "evidence 是中文散文，与真机一样"
        );
        assert!(row.path.starts_with(r"C:\tools"));
    }

    #[test]
    fn the_builder_only_sets_what_you_asked_for() {
        let bundle = bundle().source_of("tuoen.d").build();
        assert_eq!(bundle.source.as_deref(), Some("tuoen.d"));
        assert!(bundle.tools.is_none() && bundle.path.is_none());
        assert!(bundle.env.is_none() && bundle.wsl.is_none() && bundle.skipped.is_none());
        assert!(bundle.sections_present().is_empty());
    }

    #[test]
    fn skipped_entries_come_back_sorted_and_keep_their_reason() {
        let file = skipped(&[
            ("env", "ZZZ", Some("user"), "credential-named"),
            ("env", "AAA", None, "credential"),
        ]);
        let names: Vec<&str> = file.skipped.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["AAA", "ZZZ"]);
        assert!(file.skipped[0].reason.contains("AAA"));
    }

    #[test]
    fn the_temp_dir_dies_with_its_owner() {
        let path = {
            let temp = TempDir::new("fixture");
            temp.write("tools.toml", "schema_version = 1\n");
            temp.path().to_path_buf()
        };
        assert!(!path.exists(), "Drop 里应当整个删掉");
    }
}
