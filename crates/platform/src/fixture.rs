//! 固定装置：把"一台机器的状态"写成**纯数据**，让检测引擎在不碰真机的前提下可测。
//!
//! **为什么必须有它**（ticket #11 的硬约束）：测试不得读取真实的 `HKCU\Environment` /
//! `HKLM` 注册表 / 真实 `PATH` / 用户真实安装目录，也不得依赖开发机的实时状态。
//! 本机 `PATH` 恰好是坏的（48 条里有重复、有死条目、WindowsApps 别名排在第 8 条而真
//! Python 排在第 36 条）—— 用实时状态会让"测试通过"变得毫无意义。
//!
//! 三件事同时落在这里：
//! 1. [`MachineFixture`] —— `serde` 可反序列化的机器状态描述（`fixtures/detect/*.toml` 就是它）。
//! 2. 三个假后端 —— [`FakeFileSystem`] / [`FakeRegistry`] / [`FakeProcessRunner`]。
//! 3. [`FakeMachine`] —— 把假后端按 `&dyn Trait` 交给业务逻辑的那一层。
//!
//! **假后端只有读方法**（与真实实现一致），所以"这一票只读"在固定装置这一侧同样成立。

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::env_block::InMemoryEnv;
use crate::error::PlatformError;
use crate::fs_facts::{DirEntryFacts, FileFacts, FileSystem, ReparseKind};
use crate::managed::{ManagedStore, ManagedTool};
use crate::process::{ProcessOutcome, ProcessRunner};
use crate::registry::{RegHive, RegValue, Registry};

/// 一台机器的状态。字段全部有默认值，所以一个固定装置只需要写它关心的那部分 ——
/// 这也让"这个用例到底在测什么"一眼可见。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineFixture {
    /// 当前进程看到的环境变量（`ProcessEnv` 的来源）。**大小写不敏感**。
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// 独立路径的文件系统事实（不在任何已声明目录里的那些）。
    #[serde(default)]
    pub paths: Vec<FixturePath>,
    /// 目录及其内容。列目录与"路径是否存在"共用同一份声明，所以两者不可能互相矛盾。
    #[serde(default)]
    pub dirs: Vec<FixtureDir>,
    /// 注册表键。**子键由声明自动推导**，不需要单独列一遍。
    #[serde(default)]
    pub registry: Vec<FixtureKey>,
    /// 版本探测的结果。
    #[serde(default)]
    pub processes: Vec<FixtureProcess>,
    /// tuoen 自己的安装记录（`managed` 层的来源）。
    #[serde(default)]
    pub managed: Vec<ManagedTool>,
}

/// 一条路径的事实。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixturePath {
    /// 完整路径（`dirs[].entries[].path` 里则是**相对该目录的名字**，单层，不嵌套）。
    pub path: String,
    /// 存在。默认 `true` —— 写固定装置时少写一半字段。
    #[serde(default = "yes")]
    pub exists: bool,
    /// 是目录。
    #[serde(default)]
    pub is_dir: bool,
    /// 字节数。**App Execution Alias 是 0**。
    #[serde(default)]
    pub size: u64,
    /// reparse 形状。写 `"app-exec-alias"` 就是本机 `WindowsApps\python.exe` 那根东西。
    #[serde(default)]
    pub reparse: ReparseKind,
    /// 链接目标（junction / symlink）。
    #[serde(default)]
    pub link_target: Option<String>,
}

/// 手写而不是 `derive`：`serde` 那侧的 `exists` 默认是 `true`，Rust 那侧必须是同一个值，
/// 否则"用 TOML 写的固定装置"与"用 Rust 写的固定装置"会给出不同答案 —— 那种不一致
/// 会让人以为是引擎的 bug。
impl Default for FixturePath {
    fn default() -> Self {
        Self {
            path: String::new(),
            exists: true,
            is_dir: false,
            size: 0,
            reparse: ReparseKind::None,
            link_target: None,
        }
    }
}

impl FixturePath {
    /// 一个存在的普通文件。
    #[must_use]
    pub fn file(path: &str, size: u64) -> Self {
        Self {
            path: path.to_owned(),
            size,
            ..Self::default()
        }
    }

    /// 一个存在的目录。
    #[must_use]
    pub fn dir(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            is_dir: true,
            ..Self::default()
        }
    }

    /// 一个 App Execution Alias：**长度 0、tag `0x8000001b`、`exists == true`**。
    ///
    /// 本机 `…\Microsoft\WindowsApps\python.exe` 就长这样，而 `Get-Command python` 成功。
    #[must_use]
    pub fn app_exec_alias(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            size: 0,
            reparse: ReparseKind::AppExecAlias,
            ..Self::default()
        }
    }

    /// 一个目录联接。
    #[must_use]
    pub fn junction(path: &str, target: &str) -> Self {
        Self {
            path: path.to_owned(),
            is_dir: true,
            reparse: ReparseKind::Junction,
            link_target: Some(target.to_owned()),
            ..Self::default()
        }
    }

    /// 一个目录符号链接。
    #[must_use]
    pub fn symlink_dir(path: &str, target: &str) -> Self {
        Self {
            path: path.to_owned(),
            is_dir: true,
            reparse: ReparseKind::SymlinkDir,
            link_target: Some(target.to_owned()),
            ..Self::default()
        }
    }

    /// 一条**不存在**的路径（幽灵条目、失效的 `PATH` 条目）。
    #[must_use]
    pub fn missing(path: &str) -> Self {
        Self {
            path: path.to_owned(),
            exists: false,
            ..Self::default()
        }
    }
}

/// 一个目录及其内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureDir {
    /// 目录路径。
    pub path: String,
    /// 目录本身存在吗。默认 `true`。
    #[serde(default = "yes")]
    pub exists: bool,
    /// 内容。每条的 `path` 是**相对本目录的名字**。
    #[serde(default)]
    pub entries: Vec<FixturePath>,
}

impl Default for FixtureDir {
    fn default() -> Self {
        Self {
            path: String::new(),
            exists: true,
            entries: Vec::new(),
        }
    }
}

impl FixtureDir {
    /// 一个存在的目录及其内容。
    #[must_use]
    pub fn new(path: &str, entries: Vec<FixturePath>) -> Self {
        Self {
            path: path.to_owned(),
            entries,
            ..Self::default()
        }
    }
}

/// 一个注册表键。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureKey {
    /// 哪个 hive。
    pub hive: RegHive,
    /// 相对该 hive `SOFTWARE` 的子键路径。
    pub path: String,
    /// 键不存在（用来测"读不到的键"这条分支）。默认 `true`。
    #[serde(default = "yes")]
    pub exists: bool,
    /// 值名 → 值。**默认值的名字是空字符串**（`values = { "" = { sz = "C:\\x" } }`）。
    #[serde(default)]
    pub values: BTreeMap<String, RegValue>,
}

impl FixtureKey {
    /// 一个存在的键及其值。
    #[must_use]
    pub fn new(hive: RegHive, path: &str, values: BTreeMap<String, RegValue>) -> Self {
        Self {
            hive,
            path: path.to_owned(),
            exists: true,
            values,
        }
    }

    /// 一个**不存在**的键（用来测"注册表里没有这个来源"）。
    #[must_use]
    pub fn absent(hive: RegHive, path: &str) -> Self {
        Self {
            hive,
            path: path.to_owned(),
            exists: false,
            values: BTreeMap::new(),
        }
    }
}

/// 一次版本探测的结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureProcess {
    /// 可执行文件的完整路径。
    pub program: String,
    /// 标准输出。
    #[serde(default)]
    pub stdout: String,
    /// 标准错误。**版本号经常在这里**（`git --version` / `java -version`）。
    #[serde(default)]
    pub stderr: String,
    /// 退出码。
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// 模拟"这个工具不返回"。
    ///
    /// **假后端立刻返回 `timed_out`**，它证明的是"检测逻辑把超时记为发现但版本未知"；
    /// "真的超时"由 [`crate::process::SystemProcessRunner`] 的独立用例证明
    /// （真的起一个不返回的进程）。
    #[serde(default)]
    pub timed_out: bool,
}

fn yes() -> bool {
    true
}

/// 路径归一化：`/` → `\`、去掉尾部分隔符（保住裸盘符与 UNC 前缀）、小写。
///
/// Windows 的路径比较是大小写不敏感的，固定装置里的大小写不应该影响结果 ——
/// 否则一个 `C:\Tools` 与 `c:\tools` 的笔误会让用例静默变成"什么都没找到"。
#[must_use]
pub fn normalize_path(raw: &str) -> String {
    let text = raw.replace('/', "\\");
    let (prefix, rest) = match text.strip_prefix(r"\\") {
        Some(rest) => (r"\\", rest.to_owned()),
        None => ("", text),
    };
    let mut rest = rest;
    while rest.contains(r"\\") {
        rest = rest.replace(r"\\", "\\");
    }
    let trimmed = rest.trim_end_matches('\\');
    let joined = if trimmed.len() == 2 && trimmed.ends_with(':') {
        format!("{trimmed}\\")
    } else {
        trimmed.to_owned()
    };
    format!("{prefix}{joined}").to_lowercase()
}

/// 注册表键路径归一化：分隔符统一、去掉首尾分隔符、小写。
#[must_use]
fn normalize_key(raw: &str) -> String {
    raw.replace('/', "\\").trim_matches('\\').to_lowercase()
}

/// 读固定装置的假文件系统。
#[derive(Debug, Clone, Default)]
pub struct FakeFileSystem {
    index: HashMap<String, FileFacts>,
    dirs: HashMap<String, Vec<DirEntryFacts>>,
}

impl FakeFileSystem {
    fn from_fixture(fixture: &MachineFixture) -> Self {
        let mut index: HashMap<String, FileFacts> = HashMap::new();
        let mut dirs: HashMap<String, Vec<DirEntryFacts>> = HashMap::new();

        for dir in &fixture.dirs {
            let mut entries = Vec::new();
            for entry in &dir.entries {
                let full = format!("{}\\{}", dir.path.trim_end_matches('\\'), entry.path);
                let facts = entry.to_facts(Path::new(&full));
                index.insert(normalize_path(&full), facts);
                entries.push(DirEntryFacts {
                    name: entry.path.clone(),
                    is_dir: entry.is_dir,
                    reparse: entry.reparse,
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            if dir.exists {
                index.insert(
                    normalize_path(&dir.path),
                    FileFacts {
                        path: Path::new(&dir.path).to_path_buf(),
                        exists: true,
                        is_dir: true,
                        size: 0,
                        reparse: ReparseKind::None,
                        link_target: None,
                        error: None,
                    },
                );
            }
            dirs.insert(normalize_path(&dir.path), entries);
        }

        // **`paths` 覆盖 `dirs`，不是反过来。**
        //
        // 顺序在这里有语义：一条显式的 `paths` 条目说的是"**这个路径本身**是什么"
        // （一个符号链接、一个 0 字节的别名、一个不存在的东西），
        // 而 `dirs` 里的条目说的是"这个目录里**装着**什么"。
        // 两者不冲突，但之前 `paths` 先写、`dirs` 后写，于是
        // `FixturePath::symlink_dir("C:\\nvm4w\\nodejs", …)` 会被同名的
        // `FixtureDir` 覆盖成一个普通目录 —— 符号链接**静默变成目录**，
        // 而依赖 `link_target` 的检测（nvm4w 的当前版本）就再也读不到版本。
        //
        // 现在显式路径赢：`dirs` 仍然提供目录里的文件，`paths` 决定目录节点本身。
        for entry in &fixture.paths {
            let facts = entry.to_facts(Path::new(&entry.path));
            index.insert(normalize_path(&entry.path), facts);
        }

        Self { index, dirs }
    }
}

impl FixturePath {
    fn to_facts(&self, path: &Path) -> FileFacts {
        if !self.exists {
            return FileFacts::missing(path);
        }
        FileFacts {
            path: path.to_path_buf(),
            exists: true,
            is_dir: self.is_dir,
            size: self.size,
            reparse: self.reparse,
            link_target: self.link_target.clone(),
            error: None,
        }
    }
}

impl FileSystem for FakeFileSystem {
    fn inspect(&self, path: &Path) -> FileFacts {
        self.index
            .get(&normalize_path(&path.to_string_lossy()))
            .cloned()
            .unwrap_or_else(|| FileFacts::missing(path))
    }

    fn list_dir(&self, path: &Path) -> Vec<DirEntryFacts> {
        self.dirs
            .get(&normalize_path(&path.to_string_lossy()))
            .cloned()
            .unwrap_or_default()
    }
}

/// 读固定装置的假注册表。
#[derive(Debug, Clone, Default)]
pub struct FakeRegistry {
    keys: HashMap<(RegHive, String), FixtureKey>,
}

impl FakeRegistry {
    fn from_fixture(fixture: &MachineFixture) -> Self {
        let keys = fixture
            .registry
            .iter()
            .map(|key| {
                (
                    (key.hive, normalize_key(&key.path)),
                    FixtureKey {
                        path: normalize_key(&key.path),
                        ..key.clone()
                    },
                )
            })
            .collect();
        Self { keys }
    }

    fn declared(&self, hive: RegHive, key: &str) -> Option<&FixtureKey> {
        self.keys.get(&(hive, key.to_owned()))
    }

    /// 从声明里**推导**直接子键。
    ///
    /// 为什么要推导而不是让固定装置自己列一遍：父键的列表与子键的声明是同一份事实，
    /// 写两遍就会漂移（而漂移出来的假注册表会让用例测出一个真机器上不存在的形状）。
    fn derived_subkeys(&self, hive: RegHive, key: &str) -> Vec<String> {
        let prefix = if key.is_empty() {
            String::new()
        } else {
            format!("{key}\\")
        };
        let mut children: Vec<String> = self
            .keys
            .keys()
            .filter(|(child_hive, path)| {
                *child_hive == hive
                    && path.as_str() != key
                    && path.starts_with(&prefix)
                    && !path[prefix.len()..].contains('\\')
            })
            .map(|(_, path)| path[prefix.len()..].to_owned())
            .collect();
        children.sort();
        children
    }

    fn absent(hive: RegHive, key: &str) -> PlatformError {
        PlatformError::RegistryKeyMissing {
            hive: hive.as_str().to_owned(),
            subkey: key.to_owned(),
        }
    }
}

impl Registry for FakeRegistry {
    fn subkeys(&self, hive: RegHive, subkey: &str) -> Result<Vec<String>, PlatformError> {
        let key = normalize_key(subkey);
        if let Some(declared) = self.declared(hive, &key) {
            if !declared.exists {
                return Err(Self::absent(hive, &key));
            }
            return Ok(self.derived_subkeys(hive, &key));
        }
        let derived = self.derived_subkeys(hive, &key);
        if derived.is_empty() && !key.is_empty() {
            return Err(Self::absent(hive, &key));
        }
        Ok(derived)
    }

    fn values(
        &self,
        hive: RegHive,
        subkey: &str,
    ) -> Result<Vec<(String, RegValue)>, PlatformError> {
        let key = normalize_key(subkey);
        if let Some(declared) = self.declared(hive, &key) {
            if !declared.exists {
                return Err(Self::absent(hive, &key));
            }
            return Ok(declared
                .values
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect());
        }
        if self.derived_subkeys(hive, &key).is_empty() && !key.is_empty() {
            return Err(Self::absent(hive, &key));
        }
        Ok(Vec::new())
    }
}

/// 一次版本探测调用。测试用它断言**传了哪个参数**
/// （`java` 要 `-version`，`node` 要 `-v`，这件事很容易悄悄错）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCall {
    /// 程序路径。
    pub program: String,
    /// 参数。
    pub args: Vec<String>,
}

/// 读固定装置的假进程运行器。
#[derive(Debug, Clone, Default)]
pub struct FakeProcessRunner {
    entries: Vec<FixtureProcess>,
    calls: RefCell<Vec<ProcessCall>>,
}

impl FakeProcessRunner {
    fn from_fixture(fixture: &MachineFixture) -> Self {
        Self {
            entries: fixture.processes.clone(),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// 到目前为止被调用过哪些程序、带什么参数。
    #[must_use]
    pub fn calls(&self) -> Vec<ProcessCall> {
        self.calls.borrow().clone()
    }
}

impl ProcessRunner for FakeProcessRunner {
    fn run(&self, program: &Path, args: &[&str], _timeout: Duration) -> ProcessOutcome {
        let wanted = normalize_path(&program.to_string_lossy());
        self.calls.borrow_mut().push(ProcessCall {
            program: program.to_string_lossy().into_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        });
        match self
            .entries
            .iter()
            .find(|entry| normalize_path(&entry.program) == wanted)
        {
            Some(entry) => ProcessOutcome {
                spawned: true,
                timed_out: entry.timed_out,
                exit_code: entry.exit_code,
                stdout: entry.stdout.clone(),
                stderr: entry.stderr.clone(),
                spawn_error: None,
            },
            // 固定装置里没写 = 这个程序起不来。**不编一个空版本**，
            // 因为"发现但版本未知"与"根本没跑起来"在输出里必须能区分。
            None => ProcessOutcome::not_spawned("固定装置里没有这个程序"),
        }
    }
}

/// 读固定装置的假安装记录。
#[derive(Debug, Clone, Default)]
pub struct FakeManagedStore {
    tools: Vec<ManagedTool>,
}

impl ManagedStore for FakeManagedStore {
    fn installed(&self) -> Vec<ManagedTool> {
        self.tools.clone()
    }
}

/// 一台假的机器：四个后端 + 环境块。
#[derive(Debug, Clone)]
pub struct FakeMachine {
    /// 进程环境块。
    pub env: InMemoryEnv,
    /// 文件系统。
    pub fs: FakeFileSystem,
    /// 注册表。
    pub registry: FakeRegistry,
    /// 进程运行器。
    pub runner: FakeProcessRunner,
    /// tuoen 自己的安装记录。
    pub managed: FakeManagedStore,
}

impl MachineFixture {
    /// 按这份描述造一台假机器。
    #[must_use]
    pub fn build(&self) -> FakeMachine {
        FakeMachine {
            env: InMemoryEnv::new(
                self.env
                    .iter()
                    .map(|(name, value)| (name.clone(), value.clone()))
                    .collect(),
            ),
            fs: FakeFileSystem::from_fixture(self),
            registry: FakeRegistry::from_fixture(self),
            runner: FakeProcessRunner::from_fixture(self),
            managed: FakeManagedStore {
                tools: self.managed.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> MachineFixture {
        MachineFixture {
            env: BTreeMap::from([("Path".to_owned(), r"C:\machine;C:\user".to_owned())]),
            paths: vec![FixturePath::file(r"C:\machine\node.exe", 1024)],
            dirs: vec![FixtureDir::new(
                r"C:\machine",
                vec![
                    FixturePath::file("node.exe", 1024),
                    FixturePath::app_exec_alias("python.exe"),
                ],
            )],
            registry: vec![
                FixtureKey::new(
                    RegHive::Hklm,
                    r"Microsoft\Windows\CurrentVersion\App Paths",
                    BTreeMap::new(),
                ),
                FixtureKey::new(
                    RegHive::Hklm,
                    r"Microsoft\Windows\CurrentVersion\App Paths\foo.exe",
                    BTreeMap::from([(
                        String::new(),
                        RegValue::Sz(r"C:\machine\foo.exe".to_owned()),
                    )]),
                ),
            ],
            processes: vec![FixtureProcess {
                program: r"C:\machine\node.exe".to_owned(),
                stdout: "v1.2.3\n".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            }],
            managed: Vec::new(),
        }
    }

    #[test]
    fn the_fixture_is_never_confused_with_the_real_machine() {
        // 这是本文件存在的理由：假文件系统对**任何**未声明的路径都答"不存在"，
        // 所以一个忘了写固定装置的用例会失败，而不是静默读到开发机的真实磁盘。
        let machine = fixture().build();
        assert!(
            !machine
                .fs
                .inspect(Path::new(r"C:\Windows\System32\cmd.exe"))
                .exists
        );
        assert!(!machine.fs.inspect(Path::new(r"C:\Dev")).exists);
        assert!(machine.fs.list_dir(Path::new(r"C:\Windows")).is_empty());
        // 注册表也一样：只有声明过的 hive + 键存在。
        assert!(
            machine
                .registry
                .subkeys(RegHive::Hkcu, r"Microsoft\Windows\CurrentVersion\Uninstall")
                .unwrap_err()
                .is_absent()
        );
    }

    #[test]
    fn listing_and_existence_agree_because_they_share_one_declaration() {
        let machine = fixture().build();
        let entries = machine.fs.list_dir(Path::new(r"C:\machine"));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["node.exe", "python.exe"]);
        assert!(machine.fs.inspect(Path::new(r"C:\machine\node.exe")).exists);
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\machine\python.exe"))
                .is_file()
        );
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\machine\python.exe"))
                .reparse
                .is_app_exec_alias()
        );
    }

    #[test]
    fn path_lookup_is_case_insensitive_and_separator_insensitive() {
        let machine = fixture().build();
        assert!(machine.fs.inspect(Path::new("c:/MACHINE/NODE.EXE")).exists);
        assert!(
            machine
                .fs
                .inspect(Path::new(r"C:\machine\node.exe\"))
                .exists
        );
    }

    #[test]
    fn registry_subkeys_are_derived_from_the_declared_keys() {
        let machine = fixture().build();
        assert_eq!(
            machine
                .registry
                .subkeys(RegHive::Hklm, r"Microsoft\Windows\CurrentVersion\App Paths")
                .expect("父键存在"),
            vec!["foo.exe".to_owned()]
        );
        // 没声明的键 = 不存在，而且是"没有东西"而不是失败。
        let err = machine
            .registry
            .subkeys(RegHive::Hklm, r"Microsoft\Nope")
            .unwrap_err();
        assert!(err.is_absent(), "{err}");
        // 值也要读得到，且默认值的名字是空字符串。
        assert_eq!(
            machine.registry.value(
                RegHive::Hklm,
                r"Microsoft\Windows\CurrentVersion\App Paths\foo.exe",
                ""
            ),
            Some(RegValue::Sz(r"C:\machine\foo.exe".to_owned()))
        );
    }

    #[test]
    fn registry_hives_do_not_leak_into_each_other() {
        let machine = fixture().build();
        assert!(
            machine
                .registry
                .subkeys(RegHive::Hkcu, r"Microsoft\Windows\CurrentVersion\App Paths")
                .is_err(),
            "同一路径在另一个 hive 里没有声明，就必须不存在"
        );
    }

    #[test]
    fn an_undeclared_program_is_not_spawned_rather_than_invented() {
        let machine = fixture().build();
        let outcome = machine.runner.run(
            Path::new(r"C:\machine\python.exe"),
            &["--version"],
            Duration::from_millis(100),
        );
        assert!(!outcome.spawned);
        assert!(outcome.spawn_error.is_some());
        let outcome = machine.runner.run(
            Path::new(r"C:\MACHINE\node.exe"),
            &["-v"],
            Duration::from_millis(100),
        );
        assert_eq!(outcome.stdout, "v1.2.3\n");
        assert_eq!(machine.runner.calls().len(), 2);
        assert_eq!(machine.runner.calls()[1].args, vec!["-v".to_owned()]);
    }

    #[test]
    fn normalization_keeps_bare_drives_and_unc_prefixes() {
        assert_eq!(normalize_path(r"C:\Tools\"), r"c:\tools");
        assert_eq!(normalize_path(r"C:\"), r"c:\");
        assert_eq!(normalize_path("C:/"), r"c:\");
        assert_eq!(normalize_path(r"C:\\Tools\\bin"), r"c:\tools\bin");
        assert_eq!(normalize_path(r"\\srv\share\bin\"), r"\\srv\share\bin");
    }
}
