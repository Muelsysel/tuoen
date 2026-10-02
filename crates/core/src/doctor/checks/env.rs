//! `env.*` 检查 —— 持久环境变量（用户级 + 机器级）的形状。
//!
//! 输入只有 [`MachineFacts::env`]（就是 `env.toml` 的形状），**一次也不去问机器**。
//! 判据、严重度、真机实例、以及反例为什么健康，全写在各条检查旁边。
//!
//! | ID | 严重度 | 判据 | 真机实例 |
//! |---|---|---|---|
//! | `env.missing-target` | warn | `target_exists == no` | `HALCONROOT` |
//! | `env.duplicated-scope` | **error** | 同名变量在用户级与机器级各至少一条 | `NVM_HOME` / `NVM_SYMLINK` |
//! | `env.name-with-spaces` | info | 变量名里有空格 | `IntelliJ IDEA` |
//! | `env.path-literal` | warn | 值里有没有 `%` 与注册表类型对不上 | 0（它正是 `setx` 事故的指纹） |
//!
//! # 为什么这一族里只有一条 `error`
//!
//! 严重度按票据的原则分：会导致**静默失效**的是 `error`，只是噪声的是 `info`。
//! `env.duplicated-scope` 是这一族里唯一会造成静默失效的一条 —— 同一个变量被
//! 两个作用域各写了一份，**覆盖/还原只会动其中一份**，而两份在报告里都像是"当前值"。
//! 其余三条：`missing-target` 会咬人但还没咬（`warn`），`name-with-spaces` 与
//! `path-literal` 是"看一眼就好"的噪声级别（`info` / `warn`）。

use std::collections::BTreeMap;

use tuoen_platform::{EnvScope, RegType};

use super::super::{Finding, MachineFacts, Severity, ids, sources};
use crate::capture::{EnvFile, TargetExistence};

/// 跑这一族的检查。
///
/// 族内顺序固定（`diagnose` 之后还会按「严重度 + ID」整体排一遍），所以两次 `doctor`
/// 的输出逐字节相同这条性质不依赖调用顺序。
#[must_use]
pub fn run(facts: &MachineFacts) -> Vec<Finding> {
    let env = &facts.env;
    let mut findings = Vec::new();
    findings.extend(missing_targets(env));
    findings.extend(duplicated_scopes(env));
    findings.extend(names_with_spaces(env));
    findings.extend(path_literals(env));
    findings
}

/// 作用域在**人类输出**里的说法。
///
/// `evidence` 里一律用 [`EnvScope::as_str`] 的 slug（`user` / `machine`）——
/// 那个字段是数据，中文解释只在 `message` 里（见 `Finding::evidence` 的文档）。
fn scope_label(scope: EnvScope) -> &'static str {
    match scope {
        EnvScope::User => "用户",
        EnvScope::Machine => "机器",
        // 进程级变量根本不会写进 `env.toml`（见 `capture/collect/env.rs`），
        // 这一支只是为了穷尽 `match`。
        EnvScope::ProcessOnly => "进程",
    }
}

/// `RegType` 的稳定 slug。**与 TOML / `--json` 里的取值一致**（`files.rs` 用
/// `kebab-case` 序列化这个枚举），所以这里不能另编一套说法。
fn reg_type_slug(reg_type: RegType) -> &'static str {
    match reg_type {
        RegType::Sz => "sz",
        RegType::ExpandSz => "expand-sz",
    }
}

/// `env.missing-target`（warn）：变量指向的目标**看过了，那里不在**。
///
/// # 只有 `No` 算数
///
/// `target_exists` 有四态，只有 `no` 是"我们真的问了磁盘，答案是那里什么都没有"：
///
/// * `yes` —— 在，健康。
/// * `unknown` —— 值展开之后还留着 `%VAR%`。**我们没展开它，也答不上来**：
///   把含 `%` 的条目判成"失效"就是在猜（`AGENTS.md` 规矩二），本机 `PATH` 上
///   就有 `%NOPE%\bin` 这种条目。
/// * `not-a-path` —— 值根本不是一条路径（本机 `UV_PYTHON=3.13`，以及 `Path` 那种
///   `;` 列表）。把它判成"目录不存在"是**纯粹的假问题**，而一份充满假问题的报告
///   等于没有报告。
///
/// # 真机实例
///
/// `HALCONROOT` 指向 `…\MVTec\HALCON-25.11-Progress`，而 HALCON 已经卸载了 ——
/// 软件卸了、变量留着，是这一条最典型的形状。
///
/// # 反例为什么健康
///
/// `yes` / `unknown` / `not-a-path` 三种答案都在说"这条判据问不出结论"：
/// 前两种是没看过或看不了，第三种是这个问题本身不成立。**答不了的题不许猜**，
/// 所以它们一条都不报（用例钉住这件事，尤其是 `unknown`）。
///
/// # 来源为什么是 `filesystem`
///
/// 这一条的结论是"那个位置上没有东西" —— 那是文件系统的答案（`target_exists`
/// 由采集器问过磁盘）。另外三条检查只涉及注册表里的形状，所以它们的来源是 `registry`。
fn missing_targets(env: &EnvFile) -> Vec<Finding> {
    env.var
        .iter()
        .filter(|row| row.target_exists == TargetExistence::No)
        .map(|row| {
            Finding::new(
                ids::ENV_MISSING_TARGET,
                Severity::Warn,
                format!(
                    "{}级变量 {} 指向的目标不存在：它写着 {}，而那个位置上什么都没有（多半是软件卸了、变量留下了）",
                    scope_label(row.scope),
                    row.name,
                    row.value_expanded
                ),
                // evidence 是数据：作用域 + 名字 + 那个不存在的值。
                vec![format!(
                    "{} {} -> {}",
                    row.scope.as_str(),
                    row.name,
                    row.value_expanded
                )],
                sources::FILESYSTEM,
            )
        })
        .collect()
}

/// 一个名字在两个持久作用域里出现过没有。
struct ScopeHits {
    /// 报给用户看的那一份名字（同名但大小写不同时取**先出现的**那一个）。
    name: String,
    /// 用户级见过。
    user: bool,
    /// 机器级见过。
    machine: bool,
}

/// `env.duplicated-scope`（**error**）：同名变量在用户级与机器级**各至少一条**。
///
/// # 严重度为什么是 `error`
///
/// 这是**真实的双重管理腐坏**，不是理论问题：本机 `NVM_HOME` / `NVM_SYMLINK`
/// 同时存在于 `HKCU\Environment` 与 `HKLM\Session Manager\Environment`。
/// 两个作用域都写了一份，于是**覆盖/还原只会动其中一份**，而两份在报告里
/// 都像是"当前值" —— 用户看不出来自己用的是哪一个。
///
/// # 判据的三个细节
///
/// * **名字忽略大小写**：Windows 的变量名就是大小写不敏感的（`Expander` 查表也是）。
/// * **一条 finding 一个名字**，`evidence` 里两个位置都点名 ——
///   "哪两个作用域各有一份"才是用户要动手的东西。
/// * **Windows 自己在两个作用域都设的变量不算**（`WINDOWS_DEFAULT_NAMES`，决策 103）。
///
/// # 反例为什么健康
///
/// * 同名但都在同一个作用域里（不可能由采集器产出，但判据必须扛得住）：
///   没有"两个作用域抢一个名字"这件事。
/// * 不同名各占一个作用域：那是最正常的状态。
/// * 两个作用域都有的 `Path` / `TEMP` / `TMP`：Windows 的默认布局，不是腐坏。
fn duplicated_scopes(env: &EnvFile) -> Vec<Finding> {
    let mut by_name: BTreeMap<String, ScopeHits> = BTreeMap::new();
    for row in &env.var {
        let hits = by_name
            .entry(row.name.to_lowercase())
            .or_insert_with(|| ScopeHits {
                name: row.name.clone(),
                user: false,
                machine: false,
            });
        match row.scope {
            EnvScope::User => hits.user = true,
            EnvScope::Machine => hits.machine = true,
            EnvScope::ProcessOnly => {}
        }
    }

    by_name
        .into_values()
        .filter(|hits| hits.user && hits.machine)
        .filter(|hits| !is_windows_default(&hits.name))
        .map(|hits| {
            Finding::new(
                ids::ENV_DUPLICATED_SCOPE,
                Severity::Error,
                format!(
                    "变量 {} 同时被用户级与机器级设过 —— 双重管理腐坏：两份快照都说自己是对的，而覆盖/还原只会动其中一份",
                    hits.name
                ),
                // 顺序固定（先用户级、后机器级）只为让输出稳定，两次运行逐字节相同。
                vec![format!("user {}", hits.name), format!("machine {}", hits.name)],
                sources::REGISTRY,
            )
        })
        .collect()
}

/// Windows **自己**会在用户级与机器级各写一份的变量名（忽略大小写）。
///
/// 判"双重管理腐坏"问的是"有没有**两个管理者**在同一个名字上打架"，而这一批名字
/// 的管理者就是 Windows 本身 —— 它在两个作用域里各放一份是既定布局
/// （`Path` 更是 `ADR-0002` 讲 shim 的整段前提）。
///
/// # 为什么要收窄（决策 103）
///
/// 真机第一版报了 5 条，其中 3 条是假的：`Path` / `TEMP` / `TMP`。它们是**一句
/// 看起来完全合理的错话**，而 `error` 级的三条误报代价很大 —— 用户会开始怀疑
/// 这个工具（票据把这一条的严重度定成 `error`，正说明它本该稀有）。
/// 收窄之后真机剩 2 条，正是票面预期的 `NVM_HOME` / `NVM_SYMLINK`。
///
/// **名单只有这一处**：散成两份必然漂移，而漂移的后果是同一台机器上不同的检查
/// 给出不同的答案。`Path` / `TEMP` / `TMP` 是本机实测的；其余是 Windows 的既定环境。
const WINDOWS_DEFAULT_NAMES: &[&str] = &[
    "Path",
    "TEMP",
    "TMP",
    "PATHEXT",
    "ComSpec",
    "windir",
    "SystemDrive",
    "SystemRoot",
    "OS",
    "USERNAME",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    "PSModulePath",
];

/// 这个名字是不是 Windows 自己在两个作用域都设的那一批。
fn is_windows_default(name: &str) -> bool {
    WINDOWS_DEFAULT_NAMES
        .iter()
        .any(|default| default.eq_ignore_ascii_case(name))
}

/// `env.name-with-spaces`（info）：变量名里有空格。
///
/// # 真机实例
///
/// `IntelliJ IDEA` —— 名字合法（注册表不在乎），但很多工具会静默跳过它，
/// 于是"我在环境里设了啊"与"程序看不见它"同时成立。这一条就是来说这件事的。
///
/// # 严重度为什么只是 `info`
///
/// 它不会让任何东西失效，只是噪声级的提示：看一眼，知道有这么个变量存在。
/// 所以同一个名字在两个作用域里都有空格时会报两条 —— 为纯噪声去做合并不值当。
///
/// # 反例为什么健康
///
/// 名字里没有空格的变量（`JAVA_HOME`、`NVM_HOME` …）是绝大多数，
/// 它们不会命中这条判据。
fn names_with_spaces(env: &EnvFile) -> Vec<Finding> {
    env.var
        .iter()
        .filter(|row| row.name.contains(' '))
        .map(|row| {
            Finding::new(
                ids::ENV_NAME_WITH_SPACES,
                Severity::Info,
                format!(
                    "变量名 {} 里有空格 —— 这是合法的，但很多工具会静默跳过它",
                    row.name
                ),
                vec![format!("{} {}", row.scope.as_str(), row.name)],
                sources::REGISTRY,
            )
        })
        .collect()
}

/// `env.path-literal`（warn）：值里有没有 `%`，与注册表类型**对不上**。
///
/// 两个方向都报，而且 `message` 要说清是哪一种：
///
/// 1. **`sz` 的值里字面写着 `%VAR%`** —— 那个 `%VAR%` 永远不会被展开，会被当成
///    路径的一部分。这是 **`setx` 事故的典型残留**（`setx` 会把值里所有 `%VAR%`
///    永久展开成字面量，再写回去时类型已经不是 `expand-sz` 了）。
/// 2. **`expand-sz` 的值里一个 `%` 都没有** —— 不一定是错的（本机机器级 `Path`
///    就是"标了 `expand-sz` 却零变量"），但类型与内容不一致，值得看一眼。
///
/// # 与票据「本机预期 0」的一处出入（**实测**）
///
/// 票据给这条检查的本机预期是 0。按判据的方向 ②，本机实测会报 **5 条**
/// （用户级 `NVM_HOME` / `NVM_SYMLINK` / `OneDrive`，机器级 `NVM_HOME` / `NVM_SYMLINK`
/// —— 都是安装器默认写成 `REG_EXPAND_SZ` 而值里没有 `%`）。方向 ① 在本机是 0 条：
/// 用户级 `Path` 与机器级 `Path` 都是 `REG_SZ` 且不含 `%`（`GetValueKind` 实测）。
///
/// 这 5 条**没有一条是错的**：`REG_EXPAND_SZ` + 零变量是一个完全正常的组合，
/// 收窄到只有方向 ① 会让这条检查回到"看一眼就好"的噪声级别，而放宽到两个方向
/// 会让它在真机上多报 5 条。判据按票据的**文字**（"或反之"）实现，出入记在这里
/// 与交付报告里，由票据作者决定要不要收窄。
///
/// # 反例为什么健康
///
/// `sz` 且值里没有 `%`（值就是字面量）、`expand-sz` 且值里有 `%`（类型与内容一致）
/// —— 这两种是正常形状，一条都不报。
fn path_literals(env: &EnvFile) -> Vec<Finding> {
    env.var
        .iter()
        .filter_map(|row| {
            let has_percent = row.value_raw.contains('%');
            let expands = row.reg_type == RegType::ExpandSz;
            let (kind, message) = match (has_percent, expands) {
                (true, false) => (
                    "literal-percent",
                    format!(
                        "变量 {} 的值里字面写着 %…%，但它的注册表类型是 REG_SZ —— 那个 %VAR% 永远不会被展开，会被当成路径的一部分（setx 事故的典型残留）",
                        row.name
                    ),
                ),
                (false, true) => (
                    "expand-without-percent",
                    format!(
                        "变量 {} 的注册表类型是 REG_EXPAND_SZ，但它的值里一个 % 都没有 —— 不一定是错的，但类型与内容不一致，值得看一眼",
                        row.name
                    ),
                ),
                // 类型与内容一致：这一条检查要说的东西不存在。
                (true, true) | (false, false) => return None,
            };
            Some(Finding::new(
                ids::ENV_PATH_LITERAL,
                Severity::Warn,
                message,
                vec![format!(
                    "{} {} type={} kind={} value={}",
                    row.scope.as_str(),
                    row.name,
                    reg_type_slug(row.reg_type),
                    kind,
                    row.value_raw
                )],
                sources::REGISTRY,
            ))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use tuoen_platform::fixture::{FixtureDir, FixtureKey, MachineFixture};
    use tuoen_platform::{EnvScope, RegHive, RegType, RegValue};

    use crate::capture::test_support::CaptureFixture;
    use crate::capture::{
        EnvFile, EnvVarRow, PathFile, SCHEMA_VERSION, TargetExistence, ToolsFile, WslFile,
    };
    use crate::doctor::{MachineFacts, Severity, ShimFacts, SystemFacts, ids};

    use super::run;

    /// 固定的捕获时间 —— 用例里时间戳是**参数**，所以可以钉死。
    const AT: &str = "2026-10-02T12:00:00Z";

    /// 一个环境变量行，只写用例关心的字段。
    ///
    /// `value_expanded` 在这里等于 `value_raw`：这条规则（`sz` 不展开、`expand-sz` 才展开）
    /// 由采集器负责，而这一族的四条判据一条都不看展开值 —— 只有 `missing-target`
    /// 会把它印进 evidence。
    fn row(
        name: &str,
        scope: EnvScope,
        raw: &str,
        reg_type: RegType,
        target_exists: TargetExistence,
    ) -> EnvVarRow {
        EnvVarRow {
            name: name.to_owned(),
            scope,
            value_raw: raw.to_owned(),
            value_expanded: raw.to_owned(),
            reg_type,
            target: None,
            target_exists,
        }
    }

    /// 另外三个文件形状 —— 从**真的采集器**里拿一份空机器的产物。
    ///
    /// 不手写结构体：`PathFile` / `ToolsFile` / `WslFile` 会随票据加字段，
    /// 手写的那一份只会在别人加字段时静默漂移（而这一族只关心 `env`）。
    fn other_files() -> (PathFile, ToolsFile, WslFile) {
        let bundle = CaptureFixture::build(&MachineFixture::default()).capture_all(AT);
        (
            bundle.path.expect("path.toml"),
            bundle.tools.expect("tools.toml"),
            bundle.wsl.expect("wsl.toml"),
        )
    }

    /// 只有 `env` 一列有内容的「事实」。
    fn facts_with(rows: Vec<EnvVarRow>) -> MachineFacts {
        let (path, tools, wsl) = other_files();
        MachineFacts {
            path,
            env: EnvFile {
                schema_version: SCHEMA_VERSION,
                captured_at: AT.to_owned(),
                var: rows,
            },
            tools,
            wsl,
            system: SystemFacts::default(),
            resolution: Vec::new(),
            global_prefix: Vec::new(),
            dev_roots: Vec::new(),
            shims: ShimFacts::default(),
        }
    }

    /// 真机取证里的那几个变量，交给**真的采集器**跑一遍。
    ///
    /// 这条用例的价值在于：它证明下面那些手写的行不是编出来的形状 ——
    /// 同一批取值经过 `collect_env` 之后，产出的就是同一批判据要读的字段。
    /// 注册表类型也照**实测**写（`GetValueKind`）：`HALCONROOT` 与 `IntelliJ IDEA`
    /// 是 `REG_SZ`，`NVM_HOME` 两个作用域都是 `REG_EXPAND_SZ`。
    #[test]
    fn the_real_machine_shape_produces_the_same_rows_through_the_real_collector() {
        let user = FixtureKey::new(
            RegHive::Hkcu,
            "Environment",
            [
                (
                    "HALCONROOT".to_owned(),
                    RegValue::Sz(r"C:\Users\x\MVTec\HALCON-25.11-Progress".to_owned()),
                ),
                (
                    "NVM_HOME".to_owned(),
                    RegValue::ExpandSz(r"C:\Users\x\AppData\Local\nvm".to_owned()),
                ),
                (
                    "IntelliJ IDEA".to_owned(),
                    RegValue::Sz(r"C:\Program Files\JetBrains".to_owned()),
                ),
            ]
            .into(),
        );
        let machine = FixtureKey::new(
            RegHive::Hklm,
            r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment",
            [(
                "NVM_HOME".to_owned(),
                RegValue::ExpandSz(r"C:\nvm4w".to_owned()),
            )]
            .into(),
        );
        let fixture = CaptureFixture::build(&MachineFixture {
            registry: vec![user, machine],
            // 真机上这四个目标里有三个是**在**的（`NVM_HOME` 两份、`IntelliJ IDEA`），
            // 只有 HALCON 那个不在。假机器对未声明的路径一律答"不在"（这是固定装置的
            // 刻意行为），所以这里要把那三个目录声明出来，否则 `missing-target`
            // 会因为错误的原因多报三条。
            dirs: vec![
                FixtureDir::new(r"C:\Program Files\JetBrains", Vec::new()),
                FixtureDir::new(r"C:\Users\x\AppData\Local\nvm", Vec::new()),
                FixtureDir::new(r"C:\nvm4w", Vec::new()),
            ],
            ..MachineFixture::default()
        });
        let bundle = fixture.capture_all(AT);
        let mut facts = facts_with(Vec::new());
        facts.env = bundle.env.expect("env.toml");

        let findings = run(&facts);
        let by_id = |id: &str| findings.iter().filter(|finding| finding.id == id).count();
        assert_eq!(by_id(ids::ENV_MISSING_TARGET), 1, "{findings:?}");
        assert_eq!(by_id(ids::ENV_DUPLICATED_SCOPE), 1, "{findings:?}");
        assert_eq!(by_id(ids::ENV_NAME_WITH_SPACES), 1, "{findings:?}");
        // 方向 ②（`expand-sz` 里没有 `%`）：`NVM_HOME` 两个作用域各一条。
        // **真机上这一档是 5 条**（再加上 `NVM_SYMLINK` ×2 与 `OneDrive`）——
        // 都不是错的，见 `path_literals` 的文档。方向 ① 是 0 条。
        assert_eq!(by_id(ids::ENV_PATH_LITERAL), 2, "{findings:?}");

        // 报的那一条确实是 `HALCONROOT`，而且**只有**它。
        let missing = findings
            .iter()
            .find(|finding| finding.id == ids::ENV_MISSING_TARGET)
            .expect("HALCONROOT 必须被报出来");
        assert_eq!(
            missing.evidence,
            vec![r"user HALCONROOT -> C:\Users\x\MVTec\HALCON-25.11-Progress".to_owned()],
            "evidence 是数据：作用域 + 名字 + 那个不存在的值"
        );
        let duplicated = findings
            .iter()
            .find(|finding| finding.id == ids::ENV_DUPLICATED_SCOPE)
            .expect("NVM_HOME 两个作用域各一份");
        assert_eq!(
            duplicated.evidence,
            vec!["user NVM_HOME".to_owned(), "machine NVM_HOME".to_owned()]
        );
    }

    /// 正例：`target_exists = no`。
    #[test]
    fn a_target_that_is_gone_is_a_warning() {
        let facts = facts_with(vec![row(
            "HALCONROOT",
            EnvScope::User,
            r"C:\Users\x\MVTec\HALCON-25.11-Progress",
            // 真机的类型是 `REG_SZ`（`GetValueKind` 实测）—— 参数化类型很重要：
            // 写成 `expand-sz` 会同时命中 `env.path-literal` 的方向 ②。
            RegType::Sz,
            TargetExistence::No,
        )]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_MISSING_TARGET);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert_eq!(findings[0].source, crate::doctor::sources::FILESYSTEM);
        assert_eq!(
            findings[0].evidence,
            vec![r"user HALCONROOT -> C:\Users\x\MVTec\HALCON-25.11-Progress".to_owned()]
        );
        assert!(
            findings[0].message.contains("HALCONROOT"),
            "message 要点名是哪一条：{}",
            findings[0].message
        );
    }

    /// 反例：`yes` / `unknown` / `not-a-path` **一条都不许报**。
    ///
    /// 尤其是 `unknown`：它是"我们没展开、答不了"，把它算成"失效"就是猜。
    #[test]
    fn a_target_that_is_there_unresolved_or_not_a_path_is_never_reported() {
        let facts = facts_with(vec![
            row(
                "THERE",
                EnvScope::User,
                r"C:\Tools\here",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "UNRESOLVED",
                EnvScope::User,
                r"%NOPE%\bin",
                RegType::ExpandSz,
                TargetExistence::Unknown,
            ),
            row(
                "UV_PYTHON",
                EnvScope::User,
                "3.13",
                RegType::Sz,
                TargetExistence::NotAPath,
            ),
            row(
                "PSModulePath",
                EnvScope::Machine,
                r"C:\Tools\here;C:\Tools\gone",
                RegType::Sz,
                TargetExistence::NotAPath,
            ),
        ]);

        assert!(
            run(&facts).is_empty(),
            "`yes` / `unknown` / `not-a-path` 都不是「看过了，它不在」：{:?}",
            run(&facts)
        );
    }

    /// 正例：同名变量在用户级与机器级各一条（本机 `NVM_HOME` 的真实形状）。
    #[test]
    fn the_same_name_in_two_scopes_is_an_error() {
        let facts = facts_with(vec![
            row(
                "NVM_HOME",
                EnvScope::Machine,
                r"C:\nvm4w",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\Users\x\AppData\Local\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_DUPLICATED_SCOPE);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(
            findings[0].evidence,
            vec!["user NVM_HOME".to_owned(), "machine NVM_HOME".to_owned()],
            "一条 finding 一个名字，两个位置都点名"
        );
    }

    /// 名字忽略大小写地比：`nvm_home` 与 `NVM_HOME` 是同一个变量。
    #[test]
    fn the_two_scopes_are_matched_case_insensitively() {
        let facts = facts_with(vec![
            row(
                "nvm_home",
                EnvScope::Machine,
                r"C:\nvm4w",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\Users\x\AppData\Local\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_DUPLICATED_SCOPE);
    }

    /// 反例：同名但都在一个作用域里 / 不同名各占一个作用域。
    #[test]
    fn two_rows_in_one_scope_or_two_names_in_two_scopes_are_healthy() {
        let same_scope = facts_with(vec![
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\a",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\b",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);
        assert!(
            run(&same_scope).is_empty(),
            "同一个作用域里不存在「双重管理」"
        );

        let two_names = facts_with(vec![
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\a",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_SYMLINK",
                EnvScope::Machine,
                r"C:\b",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);
        assert!(run(&two_names).is_empty(), "不同名各一份是最正常的状态");
    }

    /// **收窄后的判据（决策 103）**：Windows 自己在两个作用域都设的变量不是腐坏。
    ///
    /// 真机上第一版在这里报了 3 条假的 `error`（`Path` / `TEMP` / `TMP`），
    /// 而 `Path` 在两个作用域里各有一份是**平台设计**（`ADR-0002` 的整段前提）。
    /// 这条用例同时钉住"收窄没有收过头"：真腐坏的那个名字（`NVM_HOME`）照旧报。
    #[test]
    fn a_windows_default_in_both_scopes_is_not_corruption_but_nvm_still_is() {
        let facts = facts_with(vec![
            row(
                "Path",
                EnvScope::Machine,
                r"C:\Windows",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "Path",
                EnvScope::User,
                r"%USERPROFILE%\bin",
                RegType::ExpandSz,
                TargetExistence::Yes,
            ),
            row(
                "TEMP",
                EnvScope::Machine,
                r"C:\Windows\TEMP",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "TEMP",
                EnvScope::User,
                r"%USERPROFILE%\AppData\Local\Temp",
                RegType::ExpandSz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::Machine,
                r"C:\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_DUPLICATED_SCOPE);
        assert_eq!(
            findings[0].evidence,
            vec!["user NVM_HOME".to_owned(), "machine NVM_HOME".to_owned()],
            "只报真的那一个名字"
        );
    }

    /// 名单的匹配忽略大小写（`Path` 与 `PATH` 是同一个变量）。
    #[test]
    fn the_windows_default_list_is_case_insensitive() {
        let facts = facts_with(vec![
            row(
                "PATH",
                EnvScope::Machine,
                r"C:\Windows",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "path",
                EnvScope::User,
                r"%USERPROFILE%\bin",
                RegType::ExpandSz,
                TargetExistence::Yes,
            ),
        ]);
        assert!(
            run(&facts).is_empty(),
            "`PATH` / `path` / `Path` 是同一个变量，都在名单里"
        );
    }

    /// 两个名字各自双击 —— 一条 finding 一个名字，两条不许被合并成一条。
    #[test]
    fn each_duplicated_name_gets_its_own_finding() {
        let facts = facts_with(vec![
            row(
                "NVM_HOME",
                EnvScope::Machine,
                r"C:\nvm4w",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\Users\x\AppData\Local\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_SYMLINK",
                EnvScope::Machine,
                r"C:\nvm4w\nodejs",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_SYMLINK",
                EnvScope::User,
                r"C:\nvm4w\nodejs",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 2, "{findings:?}");
        assert!(
            findings
                .iter()
                .all(|finding| finding.id == ids::ENV_DUPLICATED_SCOPE)
        );
    }

    /// 正例：变量名里有空格（本机 `IntelliJ IDEA`）。
    #[test]
    fn a_name_with_a_space_is_information_not_an_error() {
        let facts = facts_with(vec![row(
            "IntelliJ IDEA",
            EnvScope::User,
            r"C:\Program Files\JetBrains",
            RegType::Sz,
            TargetExistence::Yes,
        )]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_NAME_WITH_SPACES);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(
            findings[0].evidence,
            vec!["user IntelliJ IDEA".to_owned()],
            "名字里有空格，evidence 里也照原样带空格"
        );
    }

    /// 反例：没有空格的名字。
    #[test]
    fn a_name_without_a_space_is_not_reported() {
        let facts = facts_with(vec![
            row(
                "JAVA_HOME",
                EnvScope::Machine,
                r"C:\Dev\base\JDK\JDK8",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\Users\x\AppData\Local\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    /// 正例①：`sz` 里字面含 `%` —— `setx` 事故的指纹。
    #[test]
    fn a_literal_percent_in_a_sz_value_is_the_setx_fingerprint() {
        let facts = facts_with(vec![row(
            "BROKEN",
            EnvScope::User,
            r"%NOPE%\bin",
            RegType::Sz,
            TargetExistence::Unknown,
        )]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_PATH_LITERAL);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert!(
            findings[0].evidence[0].contains("type=sz")
                && findings[0].evidence[0].contains("kind=literal-percent"),
            "evidence 要说清是哪一种：{:?}",
            findings[0].evidence
        );
        assert!(
            findings[0].message.contains("REG_SZ"),
            "message 要说清方向：{}",
            findings[0].message
        );
    }

    /// 正例②：`expand-sz` 里一个 `%` 都没有 —— 本机 `NVM_HOME` / `OneDrive`
    /// 就是这个形状（安装器默认写成 `REG_EXPAND_SZ`，而值里没有变量）。
    #[test]
    fn an_expand_sz_value_without_a_percent_is_worth_a_look() {
        let facts = facts_with(vec![row(
            "NVM_HOME",
            EnvScope::Machine,
            r"C:\nvm4w",
            RegType::ExpandSz,
            TargetExistence::Yes,
        )]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::ENV_PATH_LITERAL);
        assert!(
            findings[0].evidence[0].contains("kind=expand-without-percent"),
            "{:?}",
            findings[0].evidence
        );
        assert!(
            findings[0].message.contains("REG_EXPAND_SZ"),
            "两个方向的 message 必须说得不一样：{}",
            findings[0].message
        );
    }

    /// 反例：类型与内容一致的两种正常形状。
    #[test]
    fn a_matching_type_and_value_is_healthy() {
        let facts = facts_with(vec![
            row(
                "PLAIN",
                EnvScope::User,
                r"C:\Tools",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "EXPANDING",
                EnvScope::User,
                r"%SystemRoot%\System32",
                RegType::ExpandSz,
                TargetExistence::Unknown,
            ),
        ]);

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    /// **`evidence` 是数据、`message` 是中文。**
    ///
    /// `--json` 的成功载荷要能被脚本按字面匹配，所以 `evidence` 里不许出现中文；
    /// 而中文解释在 `message` 里（那一列 `#[serde(skip)]`，不进 JSON）。
    /// 这条用例把两台机器上四条检查的产出一次钉住。
    #[test]
    fn evidence_is_ascii_data_while_the_message_is_chinese() {
        let facts = facts_with(vec![
            row(
                "HALCONROOT",
                EnvScope::User,
                r"C:\Users\x\MVTec\HALCON-25.11-Progress",
                RegType::Sz,
                TargetExistence::No,
            ),
            row(
                "NVM_HOME",
                EnvScope::Machine,
                r"C:\nvm4w",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\Users\x\AppData\Local\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "IntelliJ IDEA",
                EnvScope::User,
                r"C:\Program Files\JetBrains",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "BROKEN",
                EnvScope::User,
                r"%NOPE%\bin",
                RegType::Sz,
                TargetExistence::Unknown,
            ),
        ]);

        let findings = run(&facts);
        assert_eq!(findings.len(), 4, "四条检查各响一次：{findings:?}");
        for finding in &findings {
            for line in &finding.evidence {
                assert!(
                    line.is_ascii(),
                    "{} 的 evidence 里有非 ASCII：{line}",
                    finding.id
                );
            }
            assert!(
                finding.message.chars().any(|c| c as u32 > 0x2000),
                "{} 的 message 应当是中文：{}",
                finding.id,
                finding.message
            );
        }
    }

    /// 纯函数：同一份事实跑两次逐项相同（`diagnose` 的排序之外，族内自己也要稳）。
    #[test]
    fn running_twice_gives_the_same_findings() {
        let facts = facts_with(vec![
            row(
                "NVM_HOME",
                EnvScope::Machine,
                r"C:\nvm4w",
                RegType::Sz,
                TargetExistence::Yes,
            ),
            row(
                "NVM_HOME",
                EnvScope::User,
                r"C:\Users\x\AppData\Local\nvm",
                RegType::Sz,
                TargetExistence::Yes,
            ),
        ]);

        assert_eq!(run(&facts), run(&facts));
    }
}
