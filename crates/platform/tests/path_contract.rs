//! 票据 #8 的契约测试：`PATH` 的读、分析、计划、落盘。
//!
//! **一条都不碰真实的注册表、真实的磁盘。** 全部走
//! [`MachineFixture`] → [`FakeRegistry`] / [`FakeFileSystem`] / [`InMemoryEnv`]。
//! 真机上的写入机制（类型保持、广播）由 `docs/acceptance/L0-08-path.md` 里的
//! 真机探针证明 —— 那条探针写的是一个**别的值名**（`TUOEN_PATH_PROBE`），
//! 从不碰真实的 `Path`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tuoen_platform::fixture::{FixtureDir, FixtureKey, FixturePath};
use tuoen_platform::{
    BudgetLevel, EnvScope, FakeFileSystem, FakeRegistry, InMemoryEnv, MachineFixture, NoopReason,
    PATH_CLIFF_CMD, PathAction, PathChange, PlatformError, RealEnvBlock, RealFileSystem,
    RealProcessEnv, RealRegistry, RegHive, RegType, RegValue, Registry, USER_ENV_SUBKEY, analyze,
    apply, missing_from_path, plan_add, plan_remove, run, write_type_for,
};

const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";
const SHIMS: &str = r"C:\tuoen\shims";

fn user_path(value: &str, kind: RegType) -> FixtureKey {
    let value = match kind {
        RegType::Sz => RegValue::Sz(value.to_owned()),
        RegType::ExpandSz => RegValue::ExpandSz(value.to_owned()),
    };
    FixtureKey::new(
        RegHive::Hkcu,
        USER_ENV_SUBKEY,
        BTreeMap::from([("Path".to_owned(), value)]),
    )
}

fn machine_path(value: &str) -> FixtureKey {
    FixtureKey::new(
        RegHive::Hklm,
        MACHINE_ENV,
        BTreeMap::from([("Path".to_owned(), RegValue::ExpandSz(value.to_owned()))]),
    )
}

/// 造一台假机器，并把四个后端取出来。
///
/// 进程里那条 `PATH` 与注册表里的**故意可以不一致** —— 真实机器就是这样
/// （本机注册表 1703 字符、进程 1781，差的 78 是 PowerShell 的 MSIX 别名注入）。
fn machine(
    fixture: MachineFixture,
    process_path: &str,
) -> (
    RealEnvBlock<FakeRegistry, FakeFileSystem>,
    InMemoryEnv,
    FakeFileSystem,
    FakeRegistry,
) {
    let mut fixture = fixture;
    fixture
        .env
        .insert("Path".to_owned(), process_path.to_owned());
    let built = fixture.build();
    let block = RealEnvBlock::new(built.registry.clone(), built.fs.clone());
    (block, built.env, built.fs, built.registry)
}

fn analysis_of(
    fixture: MachineFixture,
    process_path: &str,
    shim_dir: Option<&Path>,
) -> tuoen_platform::PathAnalysis {
    let (block, process, fs, _registry) = machine(fixture, process_path);
    analyze(&block, &process, &fs, shim_dir)
}

// ─────────────────────────── 读 ───────────────────────────

/// 验收 ①：读出来的值是**原文**，`%USERPROFILE%` 不许被展开。
#[test]
fn reading_keeps_the_raw_value_and_does_not_expand_variables() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\Windows"),
                user_path(r"%USERPROFILE%\bin;C:\Tools", RegType::Sz),
            ],
            ..MachineFixture::default()
        },
        r"%USERPROFILE%\bin;C:\Tools",
        None,
    );

    let user = analysis
        .scopes
        .iter()
        .find(|s| s.scope == EnvScope::User)
        .expect("用户级必须在");
    assert_eq!(user.raw, r"%USERPROFILE%\bin;C:\Tools");
    assert_eq!(user.reg_type, Some(RegType::Sz));
    assert_eq!(user.entries[0].value, r"%USERPROFILE%\bin");

    // 两个作用域的顺序是**机器级在前** —— 那就是 Windows 的生效顺序。
    assert_eq!(analysis.scopes[0].scope, EnvScope::Machine);
    assert_eq!(analysis.scopes[1].scope, EnvScope::User);
}

/// 含 `%` 的条目**不许**报成失效条目：我们没展开它，报假阳性会误导用户去删。
#[test]
fn a_variable_entry_is_never_reported_as_dangling() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![user_path(r"%NOPE%\bin;C:\Gone", RegType::Sz)],
            ..MachineFixture::default()
        },
        r"%NOPE%\bin;C:\Gone",
        None,
    );

    assert_eq!(analysis.dangling.len(), 1);
    assert_eq!(analysis.dangling[0].entry.value, r"C:\Gone");
    assert!(!analysis.dangling[0].uses_variable);
}

// ─────────────────────────── 预算 ───────────────────────────

/// 验收 ⑤：档位按**生效**长度算，且两个口径确实分开。
#[test]
fn the_budget_is_measured_on_the_effective_path() {
    let injected = format!("C:\\injected;{}", "x".repeat(9000));
    let analysis = analysis_of(MachineFixture::default(), &injected, None);

    assert!(analysis.budget.effective_chars > PATH_CLIFF_CMD);
    assert_eq!(analysis.budget.level, BudgetLevel::Exceeded);
    assert_eq!(analysis.budget.remaining, 0);
    assert!(analysis.budget.level.is_alarming());
    // 注册表里一条 `Path` 都没有 —— 这一条证明两个口径确实是分开算的。
    assert_eq!(analysis.budget.registry_chars, 0);
    assert_eq!(
        analysis.process_only.len(),
        2,
        "{:?}",
        analysis.process_only
    );
}

/// 计划里的预算是**注册表口径的下界**，且它真的把机器级算进去了。
#[test]
fn the_plan_budget_counts_machine_plus_user() {
    let machine_value = "m".repeat(100);
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![
                machine_path(&machine_value),
                user_path(r"C:\a", RegType::Sz),
            ],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_add(&block, r"C:\bb").expect("不加 `;` 就该成");

    assert_eq!(plan.after_raw, r"C:\a;C:\bb");
    assert_eq!(
        plan.budget_after.effective_chars,
        100 + plan.after_raw.chars().count()
    );
    assert_eq!(plan.budget_after.level, BudgetLevel::Ok);
    assert!(plan.will_write());
}

// ─────────────────────────── 遮蔽 ───────────────────────────

/// 验收 ⑥：机器级的 `C:\nvm4w\nodejs` 遮蔽了我们的 `node.exe` shim，
/// 而 `npm`（那个目录里没有）没被遮蔽。报告要说清抢在我们前面的是哪一条。
#[test]
fn a_machine_entry_that_holds_node_exe_shadows_our_shim() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\nvm4w\nodejs"),
                user_path(SHIMS, RegType::Sz),
            ],
            dirs: vec![
                FixtureDir::new(
                    SHIMS,
                    vec![
                        FixturePath::file("node.exe", 10),
                        FixturePath::file("npm.exe", 10),
                    ],
                ),
                FixtureDir::new(r"C:\nvm4w\nodejs", vec![FixturePath::file("node.exe", 10)]),
            ],
            ..MachineFixture::default()
        },
        r"C:\nvm4w\nodejs;C:\tuoen\shims",
        Some(Path::new(SHIMS)),
    );

    assert_eq!(analysis.shadowed_shims.len(), 1, "只有 node 被遮蔽");
    let shadow = &analysis.shadowed_shims[0];
    assert_eq!(shadow.command, "node");
    assert_eq!(shadow.by.value, r"C:\nvm4w\nodejs");
    assert_eq!(shadow.by.scope, EnvScope::Machine);
    assert_eq!(shadow.file, "node.exe");
    assert_eq!(shadow.shim_dir, SHIMS);
}

/// 遮蔽看的是**生效顺序**：机器级里那条目录是空的，所以遮蔽者必须是用户级那一条。
#[test]
fn shadowing_follows_the_effective_order_not_the_scope() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\empty-first"),
                user_path(r"C:\nvm4w\nodejs;C:\tuoen\shims", RegType::Sz),
            ],
            dirs: vec![
                FixtureDir::new(SHIMS, vec![FixturePath::file("node.exe", 10)]),
                FixtureDir::new(r"C:\nvm4w\nodejs", vec![FixturePath::file("node.exe", 10)]),
                FixtureDir::new(r"C:\empty-first", Vec::new()),
            ],
            ..MachineFixture::default()
        },
        r"C:\empty-first;C:\nvm4w\nodejs;C:\tuoen\shims",
        Some(Path::new(SHIMS)),
    );

    assert_eq!(
        analysis.shadowed_shims.len(),
        1,
        "{:?}",
        analysis.shadowed_shims
    );
    assert_eq!(analysis.shadowed_shims[0].by.value, r"C:\nvm4w\nodejs");
    assert_eq!(analysis.shadowed_shims[0].by.scope, EnvScope::User);
}

/// 我们的 shim 排在前面时**一条遮蔽都不许报**。
#[test]
fn nothing_is_shadowed_when_our_shim_dir_comes_first() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![user_path(r"C:\tuoen\shims;C:\nvm4w\nodejs", RegType::Sz)],
            dirs: vec![
                FixtureDir::new(SHIMS, vec![FixturePath::file("node.exe", 10)]),
                FixtureDir::new(r"C:\nvm4w\nodejs", vec![FixturePath::file("node.exe", 10)]),
            ],
            ..MachineFixture::default()
        },
        r"C:\tuoen\shims;C:\nvm4w\nodejs",
        Some(Path::new(SHIMS)),
    );

    assert!(
        analysis.shadowed_shims.is_empty(),
        "{:?}",
        analysis.shadowed_shims
    );
}

/// shim 目录不存在（还没装）时不许报遮蔽 —— 那时我们一条命令也没发布。
#[test]
fn a_missing_shim_dir_reports_no_shadowing() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![user_path(r"C:\nvm4w\nodejs", RegType::Sz)],
            dirs: vec![FixtureDir::new(
                r"C:\nvm4w\nodejs",
                vec![FixturePath::file("node.exe", 10)],
            )],
            ..MachineFixture::default()
        },
        r"C:\nvm4w\nodejs",
        Some(Path::new(SHIMS)),
    );

    assert!(analysis.shadowed_shims.is_empty());
}

/// **生效顺序的真相**：进程里那一条才是 Windows 拼出来的东西，而本机实测的形状是
/// `[启动器注入] + 机器级 + 用户级` —— **注入项排在机器级前面**。
/// 所以"机器级在前、用户级在后"只是这个顺序的一部分，遮蔽判定必须看得见注入项。
#[test]
fn a_process_injected_directory_can_shadow_us_and_is_reported_as_such() {
    // 机器级 `C:\machine` 里**没有** node.exe；注入目录里有 —— 于是注入目录说了算。
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![machine_path(r"C:\machine"), user_path(SHIMS, RegType::Sz)],
            dirs: vec![
                FixtureDir::new(SHIMS, vec![FixturePath::file("node.exe", 10)]),
                FixtureDir::new(r"C:\machine", Vec::new()),
                FixtureDir::new(
                    r"C:\Program Files\WindowsApps\Microsoft.PowerShell_x",
                    vec![FixturePath::file("node.exe", 10)],
                ),
            ],
            ..MachineFixture::default()
        },
        r"C:\Program Files\WindowsApps\Microsoft.PowerShell_x;C:\machine;C:\tuoen\shims",
        Some(Path::new(SHIMS)),
    );

    assert_eq!(
        analysis.shadowed_shims.len(),
        1,
        "{:?}",
        analysis.shadowed_shims
    );
    assert_eq!(analysis.shadowed_shims[0].command, "node");
    assert_eq!(analysis.shadowed_shims[0].by.scope, EnvScope::ProcessOnly);
    assert_eq!(
        analysis.shadowed_shims[0].by.value,
        r"C:\Program Files\WindowsApps\Microsoft.PowerShell_x"
    );
    // 生效顺序就是进程里那个顺序，注入项在最前面。
    let order: Vec<String> = analysis
        .effective_entries()
        .iter()
        .map(|entry| entry.value.clone())
        .collect();
    assert_eq!(
        order,
        vec![
            r"C:\Program Files\WindowsApps\Microsoft.PowerShell_x".to_owned(),
            r"C:\machine".to_owned(),
            SHIMS.to_owned(),
        ]
    );
}

/// 注册表里有、进程里没有的条目（这个终端在改动之前就开了）仍然要参与遮蔽判定 ——
/// 不能因为进程里看不见就当它不存在。
#[test]
fn a_registry_entry_missing_from_this_process_is_still_considered() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\nvm4w\nodejs"),
                user_path(SHIMS, RegType::Sz),
            ],
            dirs: vec![
                FixtureDir::new(SHIMS, vec![FixturePath::file("node.exe", 10)]),
                FixtureDir::new(r"C:\nvm4w\nodejs", vec![FixturePath::file("node.exe", 10)]),
            ],
            ..MachineFixture::default()
        },
        // 进程里那条是**旧的**：既没有我们的 shim 目录，也没有 nvm 那一条。
        r"C:\Windows",
        Some(Path::new(SHIMS)),
    );

    assert_eq!(
        analysis.shadowed_shims.len(),
        1,
        "{:?}",
        analysis.shadowed_shims
    );
    assert_eq!(analysis.shadowed_shims[0].by.value, r"C:\nvm4w\nodejs");
    assert_eq!(analysis.shadowed_shims[0].by.scope, EnvScope::Machine);
}

// ─────────────────────────── 用户名依赖 ───────────────────────────

/// 验收 ⑦：硬编码用户名的条目要报出来，**并且要说清是不是在机器级**
/// （机器级的那些换机后 100% 失效，用户级的还能靠新机器上的同名用户侥幸活着）。
#[test]
fn hardcoded_usernames_are_reported_with_their_scope() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\Users\Muelsyse\java8path"),
                user_path(
                    r"C:\Users\Muelsyse\AppData\Local\nvm;%USERPROFILE%\bin",
                    RegType::Sz,
                ),
            ],
            ..MachineFixture::default()
        },
        "",
        None,
    );

    assert_eq!(analysis.username_dependencies.len(), 2);
    let machine_side = analysis
        .username_dependencies
        .iter()
        .find(|d| d.at_machine_scope)
        .expect("机器级那条必须在");
    assert_eq!(machine_side.name, "Muelsyse");
    assert_eq!(machine_side.entry.value, r"C:\Users\Muelsyse\java8path");

    let user_side = analysis
        .username_dependencies
        .iter()
        .find(|d| !d.at_machine_scope)
        .expect("用户级那条必须在");
    assert_eq!(
        user_side.entry.value,
        r"C:\Users\Muelsyse\AppData\Local\nvm"
    );
    // `%USERPROFILE%\bin` 是可移植的，不许混进这份报告。
    assert!(
        analysis
            .username_dependencies
            .iter()
            .all(|d| d.name == "Muelsyse")
    );
}

// ─────────────────────────── 重复与进程注入 ───────────────────────────

#[test]
fn a_duplicate_directory_is_reported_once_with_all_its_positions() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\Shared"),
                user_path(r"C:\Shared;C:\only-user", RegType::Sz),
            ],
            ..MachineFixture::default()
        },
        "",
        None,
    );

    assert_eq!(analysis.duplicates.len(), 1);
    assert_eq!(analysis.duplicates[0].key, r"c:\shared");
    assert_eq!(analysis.duplicates[0].at.len(), 2);
    assert_eq!(analysis.duplicates[0].at[0].scope, EnvScope::Machine);
    assert_eq!(analysis.duplicates[0].at[1].scope, EnvScope::User);
}

#[test]
fn process_only_entries_are_the_ones_the_registry_does_not_have() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![user_path(r"C:\Tools", RegType::Sz)],
            ..MachineFixture::default()
        },
        r"C:\Tools;C:\WindowsApps\alias;C:\WindowsApps\alias",
        None,
    );

    // 同一个目录在进程里出现两遍只报一遍。
    assert_eq!(
        analysis.process_only,
        vec![r"C:\WindowsApps\alias".to_owned()]
    );
}

/// `effective` 的三条语义，逐条钉住：
/// ①**去重**（同一个目录只留第一次出现）；②注册表里每条非空条目都在里面；
/// ③进程 `PATH` 里每条非空条目都在里面（包括注入项）。
///
/// 这三条比"条目数恰好等于某个式子"更值得测：后面那条只在"进程 PATH 正好是
/// 注册表那份合并结果"时成立，注入项一出现就不成立。
#[test]
fn the_effective_order_is_deduplicated_and_covers_both_sides() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![
                machine_path(r"C:\Windows;C:\a"),
                user_path(r"C:\b;C:\extra", RegType::Sz),
            ],
            ..MachineFixture::default()
        },
        // 进程里：`C:\Windows` 出现两次（去重成一个），另有一条注入项在最前面。
        r"C:\injected;C:\Windows;C:\a;C:\Windows;C:\b",
        None,
    );

    let order: Vec<String> = analysis
        .effective_entries()
        .iter()
        .map(|entry| entry.value.clone())
        .collect();

    // ① 去重：`C:\Windows` 只出现一次。顺序 = 进程顺序，注册表独有的追加在末尾。
    assert_eq!(
        order,
        vec![
            r"C:\injected".to_owned(),
            r"C:\Windows".to_owned(),
            r"C:\a".to_owned(),
            r"C:\b".to_owned(),
            r"C:\extra".to_owned(),
        ]
    );
    // ② 注册表里的每条非空条目都在。
    for scope in &analysis.scopes {
        for entry in scope.entries.iter().filter(|entry| !entry.is_empty()) {
            assert!(
                order
                    .iter()
                    .any(|value| value.eq_ignore_ascii_case(&entry.value)),
                "注册表条目 `{}` 没进 effective",
                entry.value
            );
        }
    }
    // ③ 进程里的每条非空条目都在，而且注入项带的是 `ProcessOnly` 出处。
    for need in [r"C:\injected", r"C:\Windows", r"C:\a", r"C:\b"] {
        assert!(
            order.iter().any(|value| value.eq_ignore_ascii_case(need)),
            "进程条目 `{need}` 没进 effective"
        );
    }
    let injected = analysis
        .effective_entries()
        .into_iter()
        .find(|entry| entry.value == r"C:\injected")
        .expect("注入项必须在");
    assert_eq!(injected.scope, EnvScope::ProcessOnly);
    assert_eq!(injected.index, 0, "注入项的 index 是**进程 PATH** 里的下标");
    // 注册表条目的 index 是**该作用域内**的下标（`C:\a` 是机器级第 1 段）。
    let from_registry = analysis
        .effective_entries()
        .into_iter()
        .find(|entry| entry.value == r"C:\a")
        .expect("`C:\\a` 必须在");
    assert_eq!(from_registry.scope, EnvScope::Machine);
    assert_eq!(from_registry.index, 1);
}

// ─────────────────────────── 计划：加 ───────────────────────────

#[test]
fn add_appends_and_drops_the_empty_segments_it_found() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a;;C:\b;", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_add(&block, r"C:\c").expect("ok");

    assert_eq!(plan.after_raw, r"C:\a;C:\b;C:\c");
    assert!(matches!(plan.changes[0], PathChange::Add { ref value, at: 2 } if value == r"C:\c"));
    assert!(
        plan.changes
            .iter()
            .any(|c| matches!(c, PathChange::DropEmptySegments { count: 2 }))
    );
}

/// 已经在里面了（大小写与结尾反斜杠不同）→ 不许再写一遍，也不许重排。
#[test]
fn add_is_idempotent_across_case_and_trailing_backslashes() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\Tools\;C:\b", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_add(&block, r"c:\tools").expect("ok");

    assert!(!plan.will_write(), "一个字都不该改：{plan:?}");
    // **已有条目的原文一个字都不动** —— 连那个多余的反斜杠都留着。
    assert_eq!(plan.after_raw, r"C:\Tools\;C:\b");
    assert_eq!(plan.after_raw, plan.before_raw);
    assert!(matches!(
        plan.changes[0],
        PathChange::Noop { ref value, reason: NoopReason::AlreadyPresent } if value == r"C:\Tools\"
    ));
}

/// 类型规则：加进去的目录含 `%` 就必须改成 `EXPAND_SZ`，
/// 否则那个 `%USERPROFILE%` 会被永远冻成字面量。
#[test]
fn adding_a_variable_directory_retypes_the_value() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_add(&block, r"%USERPROFILE%\.tuoen\shims").expect("ok");

    assert_eq!(plan.after_type, RegType::ExpandSz);
    assert!(matches!(
        plan.changes.last(),
        Some(PathChange::Retype {
            from: RegType::Sz,
            to: RegType::ExpandSz
        })
    ));
    assert!(plan.will_write());
}

/// 不含 `%` 时必须**保留原有类型**：本机用户级 `Path` 是 `REG_SZ`，
/// 无条件写成 `EXPAND_SZ` 就改了它的读法。
#[test]
fn a_plain_directory_keeps_the_original_type() {
    assert_eq!(write_type_for(r"C:\a", Some(RegType::Sz)), RegType::Sz);
    assert_eq!(
        write_type_for(r"C:\a", Some(RegType::ExpandSz)),
        RegType::ExpandSz
    );
    assert_eq!(write_type_for(r"C:\a", None), RegType::Sz);
}

/// `;` 不能出现在参数里 —— 引号救不了它，写进去就分成两条，且再也分不出来。
#[test]
fn an_argument_containing_a_semicolon_is_refused_instead_of_split() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );

    let add = plan_add(&block, r"C:\x;y").expect_err("必须拒绝");
    assert!(matches!(add, PlatformError::Unsupported { .. }), "{add:?}");
    let remove = plan_remove(&block, r"C:\x;y", None).expect_err("必须拒绝");
    assert!(
        matches!(remove, PlatformError::Unsupported { .. }),
        "{remove:?}"
    );
}

// ─────────────────────────── 计划：删 ───────────────────────────

#[test]
fn remove_takes_out_every_occurrence() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a;C:\b;C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_remove(&block, r"c:\A", None).expect("ok");

    assert_eq!(plan.after_raw, r"C:\b");
    let removed: Vec<usize> = plan
        .changes
        .iter()
        .filter_map(|c| match c {
            PathChange::Remove { was_at, .. } => Some(*was_at),
            _ => None,
        })
        .collect();
    assert_eq!(removed, vec![0, 2], "位置是**改之前**的下标");
}

#[test]
fn removing_something_that_is_not_there_is_a_noop() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_remove(&block, r"C:\nope", None).expect("ok");

    assert!(!plan.will_write());
    assert_eq!(plan.after_raw, r"C:\a");
    assert!(matches!(
        plan.changes[0],
        PathChange::Noop {
            reason: NoopReason::Absent,
            ..
        }
    ));
}

/// 摘掉自己的 shim 目录 = 把刚装好的命令全摘掉。这是**唯一**一个"删也不让删"的目标。
#[test]
fn removing_our_own_shim_dir_is_refused() {
    let (block, _process, _fs, _registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a;C:\tuoen\shims\;C:\b", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_remove(&block, SHIMS, Some(Path::new(SHIMS))).expect("ok");

    assert!(!plan.will_write());
    // 什么都不改，所以原文连同那个多余的反斜杠一起原样返回。
    assert_eq!(plan.after_raw, r"C:\a;C:\tuoen\shims\;C:\b");
    assert_eq!(plan.after_raw, plan.before_raw);
    assert!(matches!(
        plan.changes[0],
        PathChange::Noop {
            reason: NoopReason::ProtectedShimDir,
            ..
        }
    ));
}

// ─────────────────────────── 落盘 ───────────────────────────

/// 落盘走的是**一次写完整条值**，类型按计划；广播只发一次。
#[test]
fn apply_writes_the_whole_value_once_and_broadcasts_once() {
    let (block, _process, _fs, registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_add(&block, r"C:\b").expect("ok");

    let mut broadcast_calls = 0;
    let applied = apply(&registry, &plan, || {
        broadcast_calls += 1;
        3
    })
    .expect("写成功");

    assert!(applied.wrote);
    assert_eq!(applied.reg_type, RegType::Sz);
    assert_eq!(applied.chars, r"C:\a;C:\b".chars().count());
    assert_eq!(applied.broadcast_replies, 3);
    assert_eq!(broadcast_calls, 1);

    let stored = registry
        .value(RegHive::Hkcu, USER_ENV_SUBKEY, "Path")
        .expect("值在");
    assert_eq!(stored, RegValue::Sz(r"C:\a;C:\b".to_owned()));
}

/// 计划是空的时候 `apply` **一个字都不写、广播也不发**。
/// 广播是全局副作用（所有顶层窗口都要处理一遍），不能因为"命令跑了一次"就发。
#[test]
fn applying_a_noop_plan_writes_nothing_and_does_not_broadcast() {
    let (block, _process, _fs, registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let plan = plan_add(&block, r"C:\a").expect("ok");
    assert!(!plan.will_write());

    let mut broadcast_calls = 0;
    let applied = apply(&registry, &plan, || {
        broadcast_calls += 1;
        1
    })
    .expect("ok");

    assert!(!applied.wrote);
    assert_eq!(broadcast_calls, 0);
    assert_eq!(
        registry
            .value(RegHive::Hkcu, USER_ENV_SUBKEY, "Path")
            .unwrap(),
        RegValue::Sz(r"C:\a".to_owned())
    );
}

/// 验收 ⑧：`--dry-run` 走**同一条**计划路径，但注册表一个字节都不变。
#[test]
fn dry_run_produces_the_same_plan_without_writing() {
    let (block, _process, _fs, registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let before = registry
        .value(RegHive::Hkcu, USER_ENV_SUBKEY, "Path")
        .unwrap();

    let mut broadcast_calls = 0;
    let (plan, applied) = run(
        &block,
        &registry,
        PathAction::Add { dir: r"C:\b" },
        true,
        || {
            broadcast_calls += 1;
            0
        },
    )
    .expect("ok");

    assert!(applied.is_none(), "dry-run 不许落盘");
    assert_eq!(plan.after_raw, r"C:\a;C:\b");
    assert_eq!(broadcast_calls, 0);
    assert_eq!(
        registry
            .value(RegHive::Hkcu, USER_ENV_SUBKEY, "Path")
            .unwrap(),
        before,
        "注册表必须一个字节都不变"
    );

    // 同一条 `run` 在 `dry_run = false` 时给出**同样的计划**，只是这次真写。
    let (real_plan, real_applied) = run(
        &block,
        &registry,
        PathAction::Add { dir: r"C:\b" },
        false,
        || 1,
    )
    .expect("ok");
    assert_eq!(real_plan, plan, "dry-run 与真跑必须给出同一个计划");
    assert!(real_applied.expect("落盘了").wrote);
}

/// 落盘之后**再计划一次**就是空操作 —— 这是"幂等"的可验证形式。
#[test]
fn after_applying_the_same_request_becomes_a_noop() {
    let (block, _process, _fs, registry) = machine(
        MachineFixture {
            registry: vec![user_path(r"C:\a", RegType::Sz)],
            ..MachineFixture::default()
        },
        "",
    );
    let (_, applied) = run(
        &block,
        &registry,
        PathAction::Add { dir: r"C:\b" },
        false,
        || 1,
    )
    .unwrap();
    assert!(applied.unwrap().wrote);

    let (again, applied_again) = run(
        &block,
        &registry,
        PathAction::Add { dir: r"C:\b" },
        false,
        || panic!("空计划不许广播"),
    )
    .unwrap();
    assert!(!again.will_write());
    assert!(!applied_again.unwrap().wrote);
}

/// 用户级 `Path` 不存在 = 没设过，不是失败：加一条就是"从零建一条"。
#[test]
fn a_missing_user_path_is_treated_as_empty_not_as_an_error() {
    let (block, _process, _fs, registry) = machine(
        MachineFixture {
            registry: vec![machine_path(r"C:\Windows")],
            ..MachineFixture::default()
        },
        "",
    );
    let (plan, applied) = run(
        &block,
        &registry,
        PathAction::Add { dir: r"C:\b" },
        false,
        || 0,
    )
    .unwrap();

    assert_eq!(plan.before_raw, "");
    assert_eq!(plan.after_raw, r"C:\b");
    assert_eq!(plan.before_type, RegType::Sz);
    assert!(applied.unwrap().wrote);
}

// ─────────────────────────── 给 doctor 留的查询 ───────────────────────────

#[test]
fn missing_from_path_names_only_the_ones_that_are_really_missing() {
    let analysis = analysis_of(
        MachineFixture {
            registry: vec![user_path(r"C:\have", RegType::Sz)],
            ..MachineFixture::default()
        },
        r"C:\have",
        None,
    );

    let want = [PathBuf::from(r"C:\have"), PathBuf::from(r"C:\want")];
    let missing = missing_from_path(&analysis, &want);

    assert_eq!(missing.len(), 1);
    assert_eq!(missing[0].as_path(), Path::new(r"C:\want"));
}

// ─────────────────────────── 真机读取（只读，默认跳过） ───────────────────────────

/// 唯一的真机用例，而且**只读**。它证明整条链路在真注册表上跑得通、
/// 且不会因为机器级 `Path` 里的 `;;`、`%VAR%`、`C:\Users\<名>` 而崩。
///
/// 默认跳过：`cargo test` 不该在任何机器上都去读 HKCU。要跑就设
/// `TUOEN_REAL_MACHINE_TESTS=1`（真机验收脚本会设）。
#[test]
fn the_real_machine_reads_without_writing() {
    if std::env::var_os("TUOEN_REAL_MACHINE_TESTS").is_none() {
        eprintln!("跳过：设 TUOEN_REAL_MACHINE_TESTS=1 才跑真机读取");
        return;
    }
    let registry = RealRegistry;
    let fs = RealFileSystem;
    let block = RealEnvBlock::new(registry, fs);
    let analysis = analyze(&block, &RealProcessEnv, &fs, None);

    assert_eq!(analysis.scopes.len(), 2);
    let user = &analysis.scopes[1];
    assert_eq!(user.scope, EnvScope::User);
    println!(
        "机器级 {} 字符 / 用户级 {} 字符 / 生效 {} 字符 / 档位 {:?}",
        analysis.scopes[0].chars,
        user.chars,
        analysis.budget.effective_chars,
        analysis.budget.level
    );
    println!(
        "重复 {} 条 / 失效 {} 条 / 用户名依赖 {} 条 / 进程注入 {} 条",
        analysis.duplicates.len(),
        analysis.dangling.len(),
        analysis.username_dependencies.len(),
        analysis.process_only.len()
    );
}
