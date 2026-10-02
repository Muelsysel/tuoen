//! `pathdiff` 的**测试夹具**（不是产品代码）。
//!
//! 票据 #15 的硬约束是"测试不得读写真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH`"。
//! 这个模块提供让用例在不碰真机的前提下跑起来的那几样东西：
//!
//! * [`path_file`] / [`row`] —— 造一份 `PathFile`。存在性是**夹具给出的既定事实**
//!   （`Existence::Yes` / `No` / `Unknown`），不会被拿去问磁盘；唯一真的问磁盘的是
//!   "拼写疑似"（决策 133），而它只走 [`fake_fs`]。
//! * [`fake_fs`] / [`fake_fs_with_files`] —— 一个假的 [`FileSystem`]，
//!   对**任何**未声明的路径答"不存在"。忘了写固定装置的用例会失败，
//!   而不是静默读到开发机的真实磁盘。
//! * [`FakeEnvBlock`] —— 一个最小的 [`EnvBlock`]。
//!   平台的 `InMemoryEnv` **只**实现 `ProcessEnv`，`FakeMachine` 也不提供 `EnvBlock`，
//!   而 `RealEnvBlock` 会读真实注册表（硬约束禁止）—— 所以这里必须有它。
//! * [`diff_of`] / [`selection`] —— 让用例短一行，不引入任何新的判据。
//!
//! `path_file` 里每一条的 `expanded` 与 `value` 都只做"为了拿到值"的那一步
//! （去引号、去首尾空白），**不**做 `normalize_entry` —— 与 `capture` 的
//! `entry_value` 一致，这样 `SideRow::of` 的那一次归一化才真的被用例覆盖到。
//!
//! 这个模块**没有**"出厂二进制的后门开关"：它只在 `cfg(test)` 与别的 crate 的测试里
//! 被引用，产品代码拿不到任何"假装"。

use std::collections::BTreeMap;

use tuoen_platform::fixture::{FakeFileSystem, FixtureDir, FixturePath, MachineFixture};
use tuoen_platform::{
    EnvBlock, EnvScope, EnvVar, PathBudget, RegType, ReparseKind, hardcoded_username,
};

use crate::capture::{Existence, PathBudgetRow, PathFile, PathRow};

use super::{DiffClass, PathDiff, PathDiffOptions, Selection, compared_key, diff};

/// 夹具的捕获时间。**固定值**：用例里没有任何东西应该依赖"现在几点"。
pub const CAPTURED_AT: &str = "2026-10-02T12:00:00Z";

/// 造一行。
///
/// `index` 是**调用方给的**位置；用 [`path_file`] 时由它按作用域自动编号。
/// `expanded` / `value` 都是 `raw` 去掉一对首尾引号与首尾空白之后的形态。
#[must_use]
pub fn row(scope: EnvScope, index: usize, raw: &str, exists: Existence) -> PathRow {
    let value = entry_value(raw);
    PathRow {
        scope,
        index,
        owner: "unknown".to_owned(),
        raw: raw.to_owned(),
        expanded: value.clone(),
        quoted: is_quoted(raw),
        empty: value.trim().is_empty(),
        reg_type: Some(RegType::Sz),
        exists,
        reparse: ReparseKind::None,
        link_target: None,
        has_vars: value.contains('%'),
        has_username: hardcoded_username(&value).is_some(),
        dup_index: 0,
    }
}

/// 造一份 `PathFile`：每条给（作用域、原文、存在性）。
///
/// * `index` 按**作用域内**的出现顺序自动编号（0 起，空条目也占位）；
/// * `dup_index` 按**跨作用域**的出现顺序算，用的键与 `capture` 的 `next_dup_index`
///   同一件事（去尾部反斜杠 + 折叠大小写）—— 于是 `A;B;A` 的第三个 `A` 自然是 `1`；
/// * `budget` 按两个作用域的原文长度算（与 `capture/collect/path.rs` 同一算法）。
#[must_use]
pub fn path_file(rows: &[(EnvScope, &str, Existence)]) -> PathFile {
    let mut counters: Vec<(EnvScope, usize)> = Vec::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut entry = Vec::new();
    for (scope, raw, exists) in rows {
        let value = entry_value(raw);
        let index = next_index(&mut counters, *scope);
        let dup_index = {
            let slot = seen.entry(compared_key(&value)).or_insert(0);
            let dup = *slot;
            *slot += 1;
            dup
        };
        entry.push(PathRow {
            dup_index,
            ..row(*scope, index, raw, *exists)
        });
    }
    let file = PathFile::new(CAPTURED_AT, fixture_budget(&entry));
    PathFile { entry, ..file }
}

/// 假文件系统：每个元组是（**目录**，里的一个**子目录**名）。
///
/// 目录本身与它的子目录都会被声明成"存在且是目录"，所以
/// `C:\Software` 与 `C:\Software\tools` 都能查得到。要看文件的用例用
/// [`fake_fs_with_files`]。
#[must_use]
pub fn fake_fs(dirs: &[(&str, &str)]) -> FakeFileSystem {
    fake_fs_with_files(dirs, &[])
}

/// 同 [`fake_fs`]，外加一批**文件**（每个元组是"目录、文件的名字"）。
///
/// 拼写疑似只认目录，所以"父目录里只有同名文件"必须是**没有建议** ——
/// 这条夹具就是为那个反例准备的。
#[must_use]
pub fn fake_fs_with_files(dirs: &[(&str, &str)], files: &[(&str, &str)]) -> FakeFileSystem {
    let mut by_dir: BTreeMap<&str, Vec<FixturePath>> = BTreeMap::new();
    for (dir, child) in dirs {
        by_dir.entry(dir).or_default().push(FixturePath::dir(child));
    }
    for (dir, name) in files {
        by_dir
            .entry(dir)
            .or_default()
            .push(FixturePath::file(name, 1));
    }
    let fixture = MachineFixture {
        dirs: by_dir
            .into_iter()
            .map(|(dir, entries)| FixtureDir::new(dir, entries))
            .collect(),
        ..MachineFixture::default()
    };
    fixture.build().fs
}

/// 一个最小的假 [`EnvBlock`]：只有你声明的那几条值。
///
/// `USERPROFILE` / `USERNAME` 的真实形态就是**用户级**的，所以
/// [`FakeEnvBlock::user`] 是最常用的入口；`new` 用来钉"机器级那份不算数"。
#[derive(Debug, Clone, Default)]
pub struct FakeEnvBlock {
    vars: Vec<(EnvScope, String, String)>,
}

impl FakeEnvBlock {
    /// 逐条声明（作用域、名字、值）。
    #[must_use]
    pub fn new(vars: &[(EnvScope, &str, &str)]) -> Self {
        Self {
            vars: vars
                .iter()
                .map(|(scope, name, value)| (*scope, (*name).to_owned(), (*value).to_owned()))
                .collect(),
        }
    }

    /// 只声明用户级的若干条。
    #[must_use]
    pub fn user(vars: &[(&str, &str)]) -> Self {
        let scoped: Vec<(EnvScope, &str, &str)> = vars
            .iter()
            .map(|(name, value)| (EnvScope::User, *name, *value))
            .collect();
        Self::new(&scoped)
    }
}

impl EnvBlock for FakeEnvBlock {
    fn list(&self, scope: EnvScope) -> Vec<EnvVar> {
        self.vars
            .iter()
            .filter(|(var_scope, _, _)| *var_scope == scope)
            .map(|(scope, name, value)| EnvVar {
                name: name.clone(),
                value_raw: value.clone(),
                value_expanded: value.clone(),
                scope: *scope,
                reg_type: RegType::Sz,
                target_exists: false,
            })
            .collect()
    }

    fn get(&self, scope: EnvScope, name: &str) -> Option<EnvVar> {
        self.list(scope)
            .into_iter()
            .find(|var| var.name.eq_ignore_ascii_case(name))
    }
}

/// 跑一次 diff：**不给当前用户名**（于是没有重写）。
///
/// 需要重写的用例直接调 [`super::diff`] 并自己造 [`PathDiffOptions`] ——
/// 这个入口只是让"不关心用户名"的用例短一行。
#[must_use]
pub fn diff_of(local: &PathFile, target: &PathFile, fs: &FakeFileSystem) -> PathDiff {
    diff(
        local,
        target,
        fs,
        &PathDiffOptions {
            current_username: None,
        },
    )
}

/// 造一个选择：按类 + 按 id。
#[must_use]
pub fn selection(classes: &[DiffClass], picks: &[&str]) -> Selection {
    Selection::new(
        classes.to_vec(),
        picks.iter().map(|pick| (*pick).to_owned()).collect(),
    )
}

/// 一条条目的**值**：剥离一对首尾引号与首尾空白，其余一个字都不动。
///
/// 与 `capture/collect/path.rs` 的 `entry_value` 是同一件事，理由也一样：
/// 这里**不**做 `normalize_entry`（那是 [`super::SideRow::of`] 的事），
/// 否则 `SideRow` 的归一化就没有任何用例覆盖。
fn entry_value(raw: &str) -> String {
    let trimmed = raw.trim();
    match trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        Some(inner) => inner.to_owned(),
        None => trimmed.to_owned(),
    }
}

/// 原文是不是被一对引号包着。
fn is_quoted(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"')
}

/// 作用域内的自增下标。
fn next_index(counters: &mut Vec<(EnvScope, usize)>, scope: EnvScope) -> usize {
    if let Some(slot) = counters.iter_mut().find(|(known, _)| *known == scope) {
        let at = slot.1;
        slot.1 += 1;
        at
    } else {
        counters.push((scope, 1));
        0
    }
}

/// 两个作用域的原文长度 → 一条预算行（与 `capture/collect/path.rs` 同一算法）。
///
/// 夹具没有"进程里那条 `PATH`"，所以 `effective_chars` 用注册表口径的和 ——
/// 夹具的预算只用来让文件形状完整，用例要看档位时应当看
/// [`super::Rebuild::budget`]（那是被重建算出来的）。
fn fixture_budget(entry: &[PathRow]) -> PathBudgetRow {
    let mut raw_user_chars = 0;
    let mut raw_machine_chars = 0;
    for scope in [EnvScope::User, EnvScope::Machine] {
        let raw = entry
            .iter()
            .filter(|row| row.scope == scope)
            .map(|row| row.raw.as_str())
            .collect::<Vec<_>>()
            .join(";");
        match scope {
            EnvScope::Machine => raw_machine_chars = raw.chars().count(),
            EnvScope::User => raw_user_chars = raw.chars().count(),
            EnvScope::ProcessOnly => {}
        }
    }
    let budget = PathBudget::of(
        raw_machine_chars + raw_user_chars,
        raw_machine_chars + raw_user_chars,
    );
    PathBudgetRow {
        raw_user_chars,
        raw_machine_chars,
        effective_chars: budget.effective_chars,
        cliff: budget.cliff,
        remaining: budget.remaining,
        level: budget.level.as_str().to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::FileSystem;

    #[test]
    fn path_file_numbers_indices_per_scope_and_dups_across_scopes() {
        let file = path_file(&[
            (EnvScope::Machine, r"C:\shared", Existence::Yes),
            (EnvScope::User, r"C:\shared", Existence::Yes),
            (EnvScope::User, r"C:\other", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
        ]);
        let indices: Vec<usize> = file.entry.iter().map(|row| row.index).collect();
        assert_eq!(indices, vec![0, 0, 1, 2], "下标是**作用域内**的");
        let dups: Vec<usize> = file.entry.iter().map(|row| row.dup_index).collect();
        assert_eq!(dups, vec![0, 1, 0, 0], "dup_index 是**跨作用域**的");
        assert!(file.entry[3].empty);
        assert_eq!(file.entry[3].exists, Existence::Unknown);
    }

    #[test]
    fn path_file_strips_quotes_for_the_value_but_keeps_the_raw() {
        let file = path_file(&[(EnvScope::User, r#""C:\My Tools""#, Existence::Yes)]);
        assert_eq!(file.entry[0].raw, r#""C:\My Tools""#);
        assert_eq!(file.entry[0].expanded, r"C:\My Tools");
        assert!(file.entry[0].quoted);
        assert!(!file.entry[0].empty);
    }

    #[test]
    fn path_file_notices_a_hardcoded_username_but_not_a_variable() {
        let file = path_file(&[
            (EnvScope::User, r"C:\Users\old\bin", Existence::Yes),
            (EnvScope::User, r"%USERPROFILE%\bin", Existence::Yes),
        ]);
        assert!(file.entry[0].has_username);
        assert!(!file.entry[1].has_username);
        assert!(file.entry[1].has_vars);
    }

    #[test]
    fn the_fake_file_system_never_reads_the_real_disk() {
        let fs = fake_fs(&[(r"C:\Software", "tools")]);
        assert!(
            fs.inspect(std::path::Path::new(r"C:\Software\tools"))
                .exists
        );
        assert!(
            !fs.inspect(std::path::Path::new(r"C:\Windows\System32\cmd.exe"))
                .exists
        );
        assert!(fs.list_dir(std::path::Path::new(r"C:\Software")).len() == 1);
        assert!(fs.list_dir(std::path::Path::new(r"C:\nope")).is_empty());
    }

    #[test]
    fn files_and_directories_are_told_apart() {
        let fs = fake_fs_with_files(&[], &[(r"C:\Software", "tools")]);
        let entries = fs.list_dir(std::path::Path::new(r"C:\Software"));
        assert_eq!(entries.len(), 1);
        assert!(!entries[0].is_dir, "它是个文件，不是目录");
    }

    #[test]
    fn the_fake_env_block_answers_case_insensitively() {
        let block = FakeEnvBlock::user(&[("USERPROFILE", r"C:\Users\me")]);
        let var = block
            .get(EnvScope::User, "userprofile")
            .expect("大小写不敏感");
        assert_eq!(var.value_raw, r"C:\Users\me");
        assert!(block.get(EnvScope::Machine, "USERPROFILE").is_none());
        assert_eq!(block.list(EnvScope::User).len(), 1);
    }

    #[test]
    fn selection_keeps_the_caller_order_and_normalizes_nothing() {
        let selection = selection(&[DiffClass::Fix, DiffClass::Fix], &["user:1"]);
        assert_eq!(selection.classes().len(), 2, "夹具不替调用方去重");
        assert_eq!(selection.picks(), ["user:1".to_owned()]);
    }
}
