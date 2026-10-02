//! `tuoen restore` 的 **plan 契约测试**（票据 #16，决策 150–162）。
//!
//! # 这一份在测什么
//!
//! `plan(target, local, opts)` 是**纯函数**：两侧都是内存里的
//! [`RestoreBundle`]，不碰网络、不碰注册表、不起进程。所以这一份**完全不读这台机器**
//! —— 它读的是 `fixtures/restore/**` 那几份**自造的** `tuoen.d/` 快照。
//! 一台什么都没装的干净 Windows 上跑，结论必须与开发机上逐字相同。
//!
//! 为什么必须这样：`restore` 是**唯一**会照着另一台机器改本机的命令。它的正确性判据
//! 是"动手之前那份计划说的是不是真的"，而一条依赖开发机实时状态的测试会因为开发机的
//! 状态**时绿时红** —— 那种红绿不携带任何信息，还会诱导后来的人去改测试而不是改代码。
//!
//! # 期望值是**数出来的**，不是抄来的
//!
//! 每一节的期望条数都由这份文件**自己解析固定装置**后数出来（数条目、数作用域、
//! 数 `manager` 非空的行、数 `dup_index > 0`、自己算 `PATH` 的字符数），
//! 再与 `plan()` 的输出对照。**绝不从 `plan()` 的输出反推期望** —— 那等于拿产品
//! 自己的输出当答案，两边一起错的时候它永远是绿的。
//!
//! # 固定装置的形状（`fixtures/restore/README.md` 有完整清单）
//!
//! | 目录 | 是什么 |
//! |---|---|
//! | `machine-a/` | 目标侧：一台完整的机器 |
//! | `machine-a-current/` | 本机侧：状态与 `machine-a` **相同**，只有 `captured_at` 不同 |
//! | `machine-b-current/` | 本机侧：缺东西、也多东西 |
//! | `halfway-local/` | 本机侧：**上一次 apply 做到一半**（path/env/wsl 已落地，工具装了 3 个缺 4 个） |
//! | `path-only/` | 只有 `schema.toml` + `path.toml`（部分快照） |
//! | `empty/` | 只有 `schema.toml` 且 `sections = []` |
//!
//! # 这个文件里刻意留下的两处"看似可以更严"的地方
//!
//! 1. **假注册表"未被写"这件事不在这一份里**：`plan` 的签名里根本没有 `Registry` /
//!    `FileSystem`（它是纯函数），所以在这里写一条"假后端一个字节都没被写"的断言
//!    是**恒真**的 —— 而恒真的断言比没有断言更糟（§1.14 规矩 5：它让人以为这里有证据）。
//!    这一份断言的是**非恒真的那一半**：第三方管理器管的工具**没有任何会写的动作**，
//!    且那一节的 `counts.effective` 是空的（"真会写的条数 = 0"）。
//!    真正动到注册表/`settings.txt` 的是 `--apply` 那条路，证据在 CLI 侧。
//! 2. **"归第三方管理器管"这件事与本机有没有装它是两件事**（决策 154，口径见决策 163）：
//!    `counts.rows` 按"本机有没有"分类（已装 → `installed`），而
//!    `third-party-manager` 待办**只按目标快照里的 `manager` 字段**产出（按管理器去重）——
//!    本机已经有它并没有让"我们永远不接管它"这个问题消失。所以两侧相同时，
//!    计划里**仍然**有那两条待办。反过来（只在缺失时才报）会让"还原到本机"的真机输出
//!    **恰好漏掉**票据点名要的那条待办：本机什么工具都不缺时，它一条都不出现。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use tuoen_core::capture::{EnvFile, PathFile, SkippedFile, ToolsFile, WslFile};
use tuoen_core::restore::{
    ManualAction, ManualActionCode, NOTE_FIX_NOT_SELECTED, NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION,
    NOTE_NOT_SELECTED, NOTE_REPORT_ONLY, NOTE_SECTION_NOT_IN_SNAPSHOT, RestoreBundle, RestoreError,
    RestoreOptions, RestorePlan, SectionId, SectionPlan, SectionStatus, plan,
};
use tuoen_platform::{EnvScope, RealFileSystem};

// ─────────────────────────────────────────────────────────────────────────────
// 读固定装置
// ─────────────────────────────────────────────────────────────────────────────

/// 固定装置的根。`CARGO_MANIFEST_DIR` 是 `crates/core`，往上两级才是仓库根。
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/restore");

/// 六个目录，**顺序固定**（失败信息里的顺序也就固定了）。
const FIXTURES: [&str; 6] = [
    "machine-a",
    "machine-a-current",
    "machine-b-current",
    "halfway-local",
    "path-only",
    "empty",
];

fn fixture_dir(name: &str) -> PathBuf {
    Path::new(ROOT).join(name)
}

/// 从固定装置读一份快照。读不到就**指名道姓**地炸 —— 固定装置没了不该表现为一条
/// 看不懂的断言失败。
fn load(name: &str) -> RestoreBundle {
    let dir = fixture_dir(name);
    RestoreBundle::load(&dir, &RealFileSystem)
        .unwrap_or_else(|err| panic!("读不到固定装置 {name}（{}）：{err}", dir.display()))
}

/// 读一份固定装置里的原始文本（不经过 `RestoreBundle`）。
fn raw(name: &str, file: &str) -> String {
    let path = fixture_dir(name).join(file);
    std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("读不到 {}：{err}", path.display()))
}

/// 某一节的 `counts.rows`。
fn rows(plan: &RestorePlan, id: SectionId) -> BTreeMap<String, u64> {
    plan.section(id)
        .unwrap_or_else(|| panic!("计划里永远该有 {id:?} 这一节"))
        .counts
        .rows
        .clone()
}

/// `rows` 里某个键的条数（键不在就是 0）。
fn n(plan: &RestorePlan, id: SectionId, key: &str) -> u64 {
    rows(plan, id).get(key).copied().unwrap_or(0)
}

/// 某一节 `counts.effective` 的**总条数**（"真会写的条数"）。
fn effective_total(plan: &RestorePlan, id: SectionId) -> u64 {
    plan.section(id)
        .unwrap_or_else(|| panic!("计划里永远该有 {id:?} 这一节"))
        .counts
        .effective_total()
}

/// 某一节的动作，收成 `(id, kind)` 的集合（**不依赖顺序**）。
fn action_set(plan: &RestorePlan, id: SectionId) -> BTreeSet<(String, String)> {
    plan.section(id)
        .unwrap_or_else(|| panic!("计划里永远该有 {id:?} 这一节"))
        .actions
        .iter()
        .map(|action| (action.id.clone(), action.kind.clone()))
        .collect()
}

fn section(plan: &RestorePlan, id: SectionId) -> &SectionPlan {
    plan.section(id)
        .unwrap_or_else(|| panic!("计划里永远该有 {id:?} 这一节"))
}

/// 某一节的 `counts.rows` 的**键**（按 `BTreeMap` 的字典序）。
fn keys(plan: &RestorePlan, id: SectionId) -> Vec<String> {
    rows(plan, id).keys().cloned().collect()
}

/// 某一节里某个 `code` 的手动待办。
fn manual_of(plan: &RestorePlan, code: ManualActionCode) -> Vec<&ManualAction> {
    plan.manual_actions
        .iter()
        .filter(|action| action.code == code)
        .collect()
}

/// 整份计划的 JSON（`--json` 里 `data` 的那四个键）。纯 ASCII 那几条断言查的就是它。
fn json(plan: &RestorePlan) -> String {
    serde_json::to_string_pretty(plan).expect("计划必须能序列化成 JSON")
}

/// 一份"四个 section 都做、不带 `--with-fix`、不给当前用户名"的默认选择。
fn all() -> RestoreOptions {
    RestoreOptions::all()
}

/// 只做某一节。
fn only(id: SectionId) -> RestoreOptions {
    RestoreOptions {
        sections: vec![id],
        ..RestoreOptions::all()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 一、固定装置本身（防止夹具与代码漂移）
// ─────────────────────────────────────────────────────────────────────────────

/// 六个目录都必须能被**验收入口**解析成 `capture` 的那几个类型。
///
/// 这条用例的存在理由是"夹具与代码漂移"：`tuoen.d/` 的字段名一旦改名，
/// 集成测试如果是从 JSON 反推期望值的，就会**两边一起错**（而它永远是绿的）。
/// 这里直接走 `RestoreBundle::load`，字段名对不上就当场红。
#[test]
fn every_fixture_parses_into_the_capture_structs() {
    let mut present: BTreeMap<&'static str, Vec<String>> = BTreeMap::new();
    for name in FIXTURES {
        // `empty` 是**读不出来**的那一份（一个 section 文件都没有）—— 它有自己的用例。
        if name == "empty" {
            continue;
        }
        let bundle = load(name);
        let sections: Vec<String> = bundle
            .sections_present()
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect();
        present.insert(name, sections);
    }

    // 完整快照：四个 section 都在。
    for name in [
        "machine-a",
        "machine-a-current",
        "machine-b-current",
        "halfway-local",
    ] {
        assert_eq!(
            present[name],
            vec![
                "tools".to_owned(),
                "path".to_owned(),
                "env".to_owned(),
                "wsl".to_owned()
            ],
            "{name} 应当是一份完整快照"
        );
    }
    // 部分快照：只有 path —— 这是决策 159 的形状。
    assert_eq!(
        present["path-only"],
        vec!["path".to_owned()],
        "path-only 只该有 path 一段"
    );

    // 每一份固定装置都必须能解析出它自己声称的行数。
    let a = load("machine-a");
    assert_eq!(a.tools.as_ref().expect("tools.toml").tool.len(), 7);
    assert_eq!(a.path.as_ref().expect("path.toml").entry.len(), 6);
    assert_eq!(a.env.as_ref().expect("env.toml").var.len(), 9);
    assert_eq!(a.wsl.as_ref().expect("wsl.toml").distribution.len(), 2);
    assert_eq!(a.skipped.as_ref().expect("skipped.toml").skipped.len(), 1);
}

/// `machine-a-current` 与 `machine-a` **只差 `captured_at`**。
///
/// 这条用例是"时间戳不许造成差异"那条断言的**前提**：如果两份文件还差了别的，
/// 那条断言就会以一条看不懂的方式红掉（而不是告诉人"夹具被改坏了"）。
#[test]
fn a_fixture_variant_differs_only_in_captured_at() {
    for file in [
        "tools.toml",
        "path.toml",
        "env.toml",
        "wsl.toml",
        "skipped.toml",
    ] {
        let strip = |text: &str| -> Vec<String> {
            text.lines()
                .map(|line| {
                    if line.starts_with("captured_at = ") {
                        "captured_at = <忽略>".to_owned()
                    } else {
                        line.to_owned()
                    }
                })
                .collect()
        };
        assert_eq!(
            strip(&raw("machine-a", file)),
            strip(&raw("machine-a-current", file)),
            "{file} 除 captured_at 外必须逐字节相同"
        );
        assert_ne!(
            raw("machine-a", file),
            raw("machine-a-current", file),
            "{file} 的 captured_at 必须真的不同 —— 否则这条用例什么都没测"
        );
    }
}

/// `[budget]` 里的三个数必须与它自己的条目对得上（**自己算，不抄产品**）。
///
/// 为什么这条属于固定装置而不是产品：`[budget]` 是 `capture` 算出来的，而固定装置是
/// 手写的 —— 一个手写的字符数写错了，会让"预算"这条链上的用例集体失效，
/// 而它们红的时候指向的是产品代码。这里让夹具**先自证**。
#[test]
fn the_path_budget_block_agrees_with_the_entries_it_summarises() {
    for name in [
        "machine-a",
        "machine-b-current",
        "halfway-local",
        "path-only",
    ] {
        let bundle = load(name);
        let file: &PathFile = bundle.path.as_ref().expect("path.toml");

        let joined = |scope: EnvScope| -> usize {
            let values: Vec<&str> = file
                .entry
                .iter()
                .filter(|row| row.scope == scope)
                .map(|row| row.raw.as_str())
                .collect();
            values.join(";").chars().count()
        };
        assert_eq!(
            file.budget.raw_machine_chars,
            joined(EnvScope::Machine),
            "{name}/path.toml 的 raw_machine_chars"
        );
        assert_eq!(
            file.budget.raw_user_chars,
            joined(EnvScope::User),
            "{name}/path.toml 的 raw_user_chars"
        );

        // 生效那条 = 按 `[[effective]]` 的顺序拼起来（含进程注入项）。
        let lookup = |scope: EnvScope, index: usize| -> &str {
            file.entry
                .iter()
                .find(|row| row.scope == scope && row.index == index)
                .map_or_else(
                    || panic!("{name}/path.toml 的 [[effective]] 指向了不存在的 {scope:?}:{index}"),
                    |row| row.raw.as_str(),
                )
        };
        let effective: Vec<&str> = file
            .effective
            .iter()
            .map(|r| lookup(r.scope, r.index))
            .collect();
        assert_eq!(
            file.budget.effective_chars,
            effective.join(";").chars().count(),
            "{name}/path.toml 的 effective_chars"
        );
        assert_eq!(
            file.budget.remaining,
            file.budget.cliff - file.budget.effective_chars,
            "{name}/path.toml 的 remaining"
        );
    }
}

/// 固定装置里那些"故意造出来"的形状必须真的在（否则依赖它们的断言会静默变成空测）。
#[test]
fn the_fixtures_hold_the_shapes_the_plan_tests_count_on() {
    let a = load("machine-a");
    let tools: &ToolsFile = a.tools.as_ref().expect("tools.toml");
    let managers: BTreeSet<&str> = tools
        .tool
        .iter()
        .filter_map(|row| row.manager.as_deref())
        .collect();
    assert_eq!(
        managers,
        BTreeSet::from(["nvm4w", "uv"]),
        "两个不同的管理器"
    );
    assert_eq!(
        tools
            .tool
            .iter()
            .filter(|row| row.manager.is_some())
            .count(),
        3,
        "manager 非空的行（去重前）"
    );
    assert_eq!(
        tools.tool.iter().filter(|row| !row.reproducible).count(),
        5,
        "不可复现的行"
    );
    assert_eq!(
        tools
            .tool
            .iter()
            .filter(|row| row.confidence == "registered-missing")
            .count(),
        1,
        "占位符路径那一行"
    );
    // 中文散文与占位符都必须**真的在固定装置里**，否则"它们不许进计划"是空测。
    assert!(
        tools
            .tool
            .iter()
            .any(|row| row.evidence.contains("PATH 第 1 条")),
        "中文 evidence 必须在"
    );
    assert!(
        tools
            .tool
            .iter()
            .any(|row| row.path.contains('<') && row.path.contains('{')),
        "ARP 占位符路径必须在"
    );

    let path: &PathFile = a.path.as_ref().expect("path.toml");
    assert_eq!(
        path.entry.iter().filter(|row| row.dup_index > 0).count(),
        1,
        "本机侧/目标侧各有一条重复条目"
    );
    assert_eq!(
        path.entry
            .iter()
            .filter(|row| row.scope == EnvScope::ProcessOnly)
            .count(),
        1,
        "进程注入项必须在（它不许参与差异）"
    );
    assert_eq!(
        path.entry.iter().filter(|row| row.has_username).count(),
        3,
        "硬编码用户名的行"
    );

    let env: &EnvFile = a.env.as_ref().expect("env.toml");
    assert_eq!(
        env.var
            .iter()
            .filter(|row| row.scope == EnvScope::Machine)
            .count(),
        4
    );
    assert!(
        env.var.iter().any(|row| row.name == "CANARY_LEAKED_TOKEN"),
        "故意种下的假凭据必须在（否则'计划里不含它'是空测）"
    );

    let wsl: &WslFile = a.wsl.as_ref().expect("wsl.toml");
    assert_eq!(
        wsl.distribution
            .iter()
            .filter(|row| row.non_standard_path)
            .count(),
        1,
        "非标准位置的发行版"
    );

    let skipped: &SkippedFile = a.skipped.as_ref().expect("skipped.toml");
    assert_eq!(skipped.skipped[0].name, "ARK_API_KEY");
    assert_eq!(skipped.skipped[0].kind, "credential-named");
}

/// **真正空**的目录（一个文件都没有）也必须是 `empty-snapshot`。
///
/// 仓库里的 `fixtures/restore/empty/` 只放得下 `schema.toml`（git 不跟踪空目录），
/// 所以"一个文件都没有"这个形状只能在运行时造。
#[test]
fn a_truly_empty_directory_is_also_an_empty_snapshot() {
    let temp = tuoen_platform::test_support::TempDir::new("restore-empty");
    let err = RestoreBundle::load(temp.path(), &RealFileSystem).expect_err("空目录必须是错误");
    assert_eq!(err.code(), "empty-snapshot");

    // 同一个码也适用于"只有 schema.toml、一个 section 文件都没有"那份夹具。
    let err = RestoreBundle::load(&fixture_dir("empty"), &RealFileSystem).expect_err("空快照");
    assert_eq!(err.code(), "empty-snapshot");
    assert!(matches!(err, RestoreError::EmptySnapshot { .. }));
}

/// 目录不存在是 `snapshot-io`，**不是** `empty-snapshot` —— 用户的下一步完全不同。
#[test]
fn a_missing_directory_is_an_io_error_not_an_empty_snapshot() {
    let err = RestoreBundle::load(&fixture_dir("没有这个目录"), &RealFileSystem)
        .expect_err("不存在的目录必须是错误");
    assert_eq!(err.code(), "snapshot-io");
}

// ─────────────────────────────────────────────────────────────────────────────
// 二、两侧相同 → 接近空的计划（票据的验收原话）
// ─────────────────────────────────────────────────────────────────────────────

/// `plan(target, target)`：**四节全 `no-change`**。
///
/// 本机自己的病（这里是 `PATH` 里一条重复条目）归 `fix`，**默认不选中、只报告** ——
/// 那不是"与快照的差异"，而是本机自己的病（决策 151）。
#[test]
fn planning_against_your_own_machine_is_nearly_empty() {
    let target = load("machine-a");
    let local = load("machine-a-current");
    let p = plan(&target, &local, &all());

    for id in SectionId::ALL {
        assert_eq!(
            section(&p, id).status,
            SectionStatus::NoChange,
            "{id:?} 两侧相同时不该有任何变更"
        );
    }
    assert!(!p.has_changes());

    // tools：7 行全部只计数（同名同版本 = 本机已经有了）。
    assert_eq!(n(&p, SectionId::Tools, "installed"), 7);
    assert_eq!(n(&p, SectionId::Tools, "missing"), 0);
    assert_eq!(n(&p, SectionId::Tools, "third-party"), 0);
    assert_eq!(n(&p, SectionId::Tools, "unsupported"), 0);
    assert_eq!(n(&p, SectionId::Tools, "extra"), 0);
    assert!(section(&p, SectionId::Tools).actions.is_empty());
    assert_eq!(effective_total(&p, SectionId::Tools), 0);

    // path：4 条 keep + **本机自己的一条重复**（fix），fix 不选中 → 零动作、零落地。
    assert_eq!(n(&p, SectionId::Path, "keep"), 4);
    assert_eq!(n(&p, SectionId::Path, "fix"), 1);
    assert_eq!(n(&p, SectionId::Path, "add"), 0);
    assert_eq!(n(&p, SectionId::Path, "remove"), 0);
    assert_eq!(n(&p, SectionId::Path, "move"), 0);
    assert_eq!(n(&p, SectionId::Path, "caseOnly"), 0);
    assert!(
        section(&p, SectionId::Path).actions.is_empty(),
        "fix 默认不选中 —— 报出来但不动它"
    );
    assert_eq!(effective_total(&p, SectionId::Path), 0);
    assert_eq!(
        section(&p, SectionId::Path).note.as_deref(),
        Some(NOTE_FIX_NOT_SELECTED)
    );

    // env：9 条都在；凭据那条照旧只报"你自己重配"。
    assert_eq!(n(&p, SectionId::Env, "present"), 9);
    assert_eq!(n(&p, SectionId::Env, "missing-user"), 0);
    assert_eq!(n(&p, SectionId::Env, "missing-machine"), 0);
    assert_eq!(n(&p, SectionId::Env, "secret-skipped"), 1);
    assert_eq!(
        action_set(&p, SectionId::Env),
        BTreeSet::from([("env:ARK_API_KEY".to_owned(), "skipped-secret".to_owned())])
    );
    assert_eq!(
        effective_total(&p, SectionId::Env),
        0,
        "被跳过的凭据**绝不**进'要写'的清单（决策 153）"
    );

    // wsl：只报告。
    assert_eq!(n(&p, SectionId::Wsl, "same"), 2);
    assert_eq!(
        section(&p, SectionId::Wsl).note.as_deref(),
        Some(NOTE_REPORT_ONLY)
    );
    assert!(section(&p, SectionId::Wsl).actions.is_empty());
    assert_eq!(effective_total(&p, SectionId::Wsl), 0);

    // 摘要：四节 no-change；手动待办 = 两个管理器 + 凭据那一条。
    assert_eq!(p.summary.sections, 4);
    assert_eq!(p.summary.no_change, 4);
    assert_eq!(p.summary.would_change, 0);
    assert_eq!(p.summary.requires_elevation, 0);
    assert_eq!(p.summary.needs_network, 0);
    assert_eq!(p.summary.unsupported, 0);
    assert_eq!(p.summary.skipped, 0);

    // **两侧相同时也要报"这些工具归别人管"**：那是我们**永远**不做的事，
    // 不是"这一次的差异"。按管理器去重：`nvm4w` 在目标里管着两行，只出一条。
    // **两侧相同时也要报"这些工具归别人管"**（决策 163）：那是我们**永远**不做的事，
    // 不是"这一次的差异"。按管理器去重：`nvm4w` 在目标里管着两行，只出一条。
    assert_eq!(p.summary.manual_actions, 3, "{}", json(&p));
    assert_eq!(p.manual_actions.len(), 3);
    let managers: BTreeSet<&str> = manual_of(&p, ManualActionCode::ThirdPartyManager)
        .iter()
        .map(|action| action.subject.as_str())
        .collect();
    assert_eq!(managers, BTreeSet::from(["nvm4w", "uv"]));
    assert_eq!(
        manual_of(&p, ManualActionCode::CredentialReconfigure).len(),
        1
    );
    assert_eq!(
        manual_of(&p, ManualActionCode::RequiresElevation).len(),
        0,
        "两侧相同时没有机器级写入"
    );
}

/// `captured_at` 是**元数据**：它一变，计划必须一个字节都不变。
///
/// `machine-a-current` 与 `machine-a` 的状态逐字节相同、只有时间戳不同（夹具那条用例
/// 钉住了这个前提），所以两份计划的 JSON 必须**逐字节相同**。
#[test]
fn captured_at_never_changes_the_plan() {
    let target = load("machine-a");
    let current = load("machine-a-current");
    assert_ne!(
        target.captured_at(),
        current.captured_at(),
        "前提：两份快照的时间戳必须真的不同"
    );

    let with_other_timestamp = plan(&target, &current, &all());
    let with_itself = plan(&target, &target, &all());
    assert_eq!(json(&with_other_timestamp), json(&with_itself));
}

/// 每一节的 `counts.rows` **键全部出现（含 0）**，形状稳定（决策 158）。
///
/// 顺序是 `BTreeMap` 的字典序（稳定，但不是文档里那张表的顺序）—— 所以这里比的是
/// **键的集合**，键序由 `BTreeMap` 自己保证。
#[test]
fn every_section_reports_all_of_its_keys_even_when_zero() {
    let p = plan(&load("machine-a"), &load("machine-a-current"), &all());
    assert_eq!(
        keys(&p, SectionId::Tools),
        [
            "extra",
            "installed",
            "missing",
            "third-party",
            "unsupported"
        ]
    );
    assert_eq!(
        keys(&p, SectionId::Path),
        ["add", "caseOnly", "fix", "keep", "move", "remove"]
    );
    assert_eq!(
        keys(&p, SectionId::Env),
        [
            "missing-machine",
            "missing-user",
            "present",
            "secret-skipped"
        ]
    );
    assert_eq!(
        keys(&p, SectionId::Wsl),
        ["extra", "missing", "path-differs", "same"]
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// 三、本机缺东西 → 四条路各走各的（决策 160）
// ─────────────────────────────────────────────────────────────────────────────

/// 缺失的工具分四路，**先命中先算**：第三方管理器 → 装 → 许可被拒 → 这个版本不支持。
///
/// 期望条数是数出来的：固定装置里 7 行工具，本机侧有 2 行同名同版本、
/// 1 行本机独有；缺的 5 行按四路拆成 1 装 / 2 第三方 / 2 不支持。
#[test]
fn a_tool_that_is_missing_locally_is_split_into_the_four_routes() {
    let target = load("machine-a");
    let local = load("machine-b-current");
    let p = plan(&target, &local, &all());
    let t = SectionId::Tools;

    assert_eq!(n(&p, t, "installed"), 2, "cargo + node 22.11.0");
    assert_eq!(n(&p, t, "missing"), 1, "只有 git 会真装");
    assert_eq!(n(&p, t, "third-party"), 2, "node 24.19.0 + uv");
    assert_eq!(n(&p, t, "unsupported"), 2, "oracle-jdk + python");
    assert_eq!(n(&p, t, "extra"), 1, "本机独有 dotnet —— 从不卸载");

    assert_eq!(section(&p, t).status, SectionStatus::NeedsNetwork);
    assert!(section(&p, t).needs_network, "有要下载的东西");
    assert!(
        !section(&p, t).requires_elevation,
        "工具装进我们自己的存储，不提权"
    );
    assert_eq!(
        section(&p, t).counts.effective,
        BTreeMap::from([("install".to_owned(), 1)]),
        "真会写的只有 git 一条"
    );

    assert_eq!(
        action_set(&p, t),
        BTreeSet::from([
            ("tools:git".to_owned(), "install".to_owned()),
            ("tools:node".to_owned(), "third-party-managed".to_owned()),
            ("tools:oracle-jdk".to_owned(), "unsupported".to_owned()),
            ("tools:python".to_owned(), "unsupported".to_owned()),
            ("tools:uv".to_owned(), "third-party-managed".to_owned()),
        ])
    );

    // `detail` 是稳定 slug 的数据（`recipe=` / `version=` / `manager=`），不是散文。
    let detail_of = |id: &str| -> String {
        section(&p, t)
            .actions
            .iter()
            .find(|action| action.id == id)
            .map_or_else(
                || panic!("没有 {id} 这条动作"),
                |action| action.detail.clone(),
            )
    };
    assert_eq!(detail_of("tools:git"), "recipe=git version=2.51.0");
    assert_eq!(
        detail_of("tools:node"),
        "recipe=node version=24.19.0 manager=nvm4w"
    );
}

/// 第三方版本管理器管的工具：**只有"我们不管"这一条动作，没有任何会写的动作**。
///
/// 决策 154：nvm4w / uv 的符号链接、环境变量、`settings.txt` 我们都不碰。
/// `plan` 是纯函数（签名里没有 `Registry` / `FileSystem`），所以"假后端一个字节都没被写"
/// 这件事在这一层是**恒真**的 —— 真正有牙齿的断言是下面这条：那几行**没有任何会写的动作**，
/// 且这一节"真会写的条数"里没有它们。
#[test]
fn a_third_party_managed_tool_has_no_writing_action() {
    let target = load("machine-a");
    let p = plan(&target, &load("machine-b-current"), &all());
    let t = SectionId::Tools;

    const WRITING_KINDS: [&str; 6] = [
        "install",
        "set-user",
        "set-machine",
        "add",
        "remove",
        "move",
    ];

    // 目标里被 nvm4w / uv 管着的三行（node 22.11.0 / node 24.19.0 / uv 0.5.0）：
    // 它们要么只计数（本机已有），要么只出 `third-party-managed`。
    for action in &section(&p, t).actions {
        if action.subject == "node" || action.subject == "uv" {
            assert_eq!(
                action.kind, "third-party-managed",
                "{} 只许是'我们不管'，不许有会写的动作",
                action.id
            );
            assert!(
                !WRITING_KINDS.contains(&action.kind.as_str()),
                "{} 是会写的动作",
                action.id
            );
        }
    }

    // 会真装的那一条**只有** git（它没有 manager）。
    let installs: Vec<&str> = section(&p, t)
        .actions
        .iter()
        .filter(|action| action.kind == "install")
        .map(|action| action.subject.as_str())
        .collect();
    assert_eq!(installs, vec!["git"]);
}

/// 许可被拒与"这个版本不支持"**必须是两个不同的 code、两个不同的具体 slug**。
///
/// 票据点名：许可那一条要给出**具体原因**，不是"许可问题"这种无用信息（决策 160）。
#[test]
fn a_licence_blocked_tool_names_the_concrete_reason() {
    let p = plan(&load("machine-a"), &load("machine-b-current"), &all());

    let blocked = manual_of(&p, ManualActionCode::LicenceBlocked);
    assert_eq!(blocked.len(), 1, "表里当前只有 oracle-jdk 一项");
    assert_eq!(blocked[0].subject, "oracle-jdk");
    assert_eq!(
        blocked[0].detail, "oracle-jdk-redistribution-not-permitted",
        "要具体到'再分发不允许'，不是 'licence' 也不是空串"
    );
    assert_eq!(blocked[0].remediation, "install-manually");

    let unsupported = manual_of(&p, ManualActionCode::Unsupported);
    assert_eq!(unsupported.len(), 1);
    assert_eq!(unsupported[0].subject, "python", "不可复现但不是许可问题");
    assert_eq!(unsupported[0].detail, "not-reproducible");
    assert_eq!(unsupported[0].remediation, "not-supported-in-this-version");

    // 两条的 code 与 detail 都不许混。
    assert_ne!(blocked[0].code, unsupported[0].code);
    assert_ne!(blocked[0].detail, unsupported[0].detail);
}

/// 五类手动待办各至少一条，而 `third-party-manager` **按管理器去重**。
///
/// 固定装置里 `manager` 非空的行有 3 行（node ×2 + uv），但不同的管理器只有 2 个
/// （`nvm4w`、`uv`）—— 所以待办是 2 条，不是 3 条。
#[test]
fn every_manual_action_class_appears_and_managers_are_deduped() {
    let p = plan(&load("machine-a"), &load("machine-b-current"), &all());

    let mut by_code: BTreeMap<&str, usize> = BTreeMap::new();
    for action in &p.manual_actions {
        *by_code.entry(action.code.as_str()).or_insert(0) += 1;
    }
    assert_eq!(
        by_code,
        BTreeMap::from([
            ("credential-reconfigure", 1),
            ("licence-blocked", 1),
            ("requires-elevation", 1),
            ("third-party-manager", 2),
            ("unsupported", 1),
        ]),
        "五类各至少一条；third-party-manager 按管理器去重成 2 条"
    );
    assert_eq!(p.summary.manual_actions, 6);

    let managers: BTreeSet<&str> = manual_of(&p, ManualActionCode::ThirdPartyManager)
        .iter()
        .map(|action| action.subject.as_str())
        .collect();
    assert_eq!(managers, BTreeSet::from(["nvm4w", "uv"]));
    for action in manual_of(&p, ManualActionCode::ThirdPartyManager) {
        assert_eq!(action.detail, action.subject, "detail 也是管理器名");
        assert_eq!(action.remediation, "use-the-manager");
    }
}

/// 凭据那条待办：`subject` 是**标识**、`detail` 是 kind 的 slug，材料一个字节都没有。
#[test]
fn a_skipped_credential_is_a_todo_without_any_material() {
    let p = plan(&load("machine-a"), &load("machine-b-current"), &all());
    let todo = manual_of(&p, ManualActionCode::CredentialReconfigure);
    assert_eq!(todo.len(), 1);
    assert_eq!(todo[0].subject, "env:ARK_API_KEY");
    assert_eq!(todo[0].detail, "credential-named");
    assert_eq!(todo[0].remediation, "reconfigure-manually");
}

/// 机器级环境变量：报 `requires-elevation`，**不自己提权**（决策 12/136）。
#[test]
fn a_machine_scope_variable_asks_for_elevation_instead_of_doing_it() {
    let p = plan(&load("machine-a"), &load("machine-b-current"), &all());
    let e = SectionId::Env;

    assert_eq!(n(&p, e, "present"), 7);
    assert_eq!(n(&p, e, "missing-user"), 1, "NVM_HOME 的用户级那一份缺了");
    assert_eq!(n(&p, e, "missing-machine"), 1, "M2_HOME 缺了");
    assert_eq!(n(&p, e, "secret-skipped"), 1);
    assert_eq!(effective_total(&p, e), 2, "set-user + set-machine");

    assert_eq!(section(&p, e).status, SectionStatus::RequiresElevation);
    assert!(section(&p, e).requires_elevation);
    assert!(!section(&p, e).needs_network, "写环境变量不需要下载");
    assert_eq!(
        section(&p, e).note.as_deref(),
        Some(NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION)
    );
    assert_eq!(
        action_set(&p, e),
        BTreeSet::from([
            ("env:user:NVM_HOME".to_owned(), "set-user".to_owned()),
            ("env:machine:M2_HOME".to_owned(), "set-machine".to_owned()),
            ("env:ARK_API_KEY".to_owned(), "skipped-secret".to_owned()),
        ])
    );

    let elevation = manual_of(&p, ManualActionCode::RequiresElevation);
    assert_eq!(elevation.len(), 1);
    assert_eq!(elevation[0].subject, "M2_HOME");
    assert_eq!(elevation[0].detail, "scope=machine var=M2_HOME");
    assert_eq!(elevation[0].remediation, "run-as-administrator");
}

// ─────────────────────────────────────────────────────────────────────────────
// 四、PATH 的差异（§1.17 的六类 + 决策 158 的两个口径）
// ─────────────────────────────────────────────────────────────────────────────

/// `PATH` 的差异六类都要报，而 `counts.effective` 是**真会写下去的条数**。
///
/// 目标侧自己带了一条重复条目（`C:\Users\dev\.cargo\bin` 出现两次）：它在本机没有对应的
/// 第 2 次出现，所以归 `add` —— 于是 `rows.add == 2`，而**只有 1 条真会落地**
/// （另一条被"输出里不许有折叠后重复的条目"这条不变量跳过，决策 158 的真机实测形状）。
#[test]
fn path_differences_report_both_calibers() {
    let target = load("machine-a");
    let local = load("machine-b-current");
    let p = plan(&target, &local, &all());
    let d = SectionId::Path;

    assert_eq!(
        rows(&p, d),
        BTreeMap::from([
            ("keep".to_owned(), 3),
            ("add".to_owned(), 2),
            ("remove".to_owned(), 1),
            ("move".to_owned(), 0),
            ("fix".to_owned(), 0),
            ("caseOnly".to_owned(), 0),
        ]),
        "进程注入项不参与比较（决策 126）：两侧的注入项故意不同，一个数都不该进"
    );
    assert_eq!(section(&p, d).status, SectionStatus::WouldChange);
    assert!(!section(&p, d).requires_elevation, "这一节的写入全在用户级");
    assert_eq!(section(&p, d).note, None, "本机侧没有 fix");

    assert_eq!(
        action_set(&p, d),
        BTreeSet::from([
            ("user:+1".to_owned(), "add".to_owned()),
            ("user:+2".to_owned(), "add".to_owned()),
            ("user:1".to_owned(), "remove".to_owned()),
        ])
    );

    // 两个口径真的不一样：2 条 add，1 条落地。
    assert_eq!(
        section(&p, d).counts.effective,
        BTreeMap::from([("add".to_owned(), 1), ("remove".to_owned(), 1)])
    );

    // 那条不会落地的 add 的 `detail` 是 `no-op` —— 它被输出不变量跳过了。
    let skipped_insert = section(&p, d)
        .actions
        .iter()
        .find(|action| action.id == "user:+2")
        .expect("目标自带的重复条目会产出一条 add");
    assert_eq!(skipped_insert.kind, "add");
    assert_eq!(skipped_insert.detail, "no-op", "它不落地，所以没有 toIndex");
}

// ─────────────────────────────────────────────────────────────────────────────
// 五、`--only` 与部分快照（决策 159/161：两句"没做"必须分得开）
// ─────────────────────────────────────────────────────────────────────────────

/// `--only path`：另外三节是 `skipped` + `not-selected`，**不是**"一切正常"。
#[test]
fn a_section_that_was_not_selected_is_skipped_with_its_own_note() {
    let p = plan(
        &load("machine-a"),
        &load("machine-b-current"),
        &only(SectionId::Path),
    );

    for id in [SectionId::Tools, SectionId::Env, SectionId::Wsl] {
        assert_eq!(section(&p, id).status, SectionStatus::Skipped, "{id:?}");
        assert_eq!(
            section(&p, id).note.as_deref(),
            Some(NOTE_NOT_SELECTED),
            "{id:?}"
        );
        assert!(section(&p, id).actions.is_empty(), "{id:?} 不该有动作");
        assert!(
            section(&p, id).counts.rows.is_empty(),
            "{id:?} 没参与，就不印一张全 0 的表"
        );
    }

    assert_eq!(
        section(&p, SectionId::Path).status,
        SectionStatus::WouldChange
    );
    assert_eq!(p.summary.skipped, 3);
    assert_eq!(p.summary.would_change, 1);
    assert!(
        p.manual_actions.is_empty(),
        "只做 path 时，env 的机器级提权待办不该出现"
    );
}

/// 快照里没有这一节 → `skipped` + `section-not-in-snapshot`（决策 159）。
///
/// 与"没被 `--only` 选中"是**两句话**：前者是"你给我的快照里没有这一节"，
/// 后者是"你没让我做这一节"。用户的下一步完全不同。
///
/// 注意这里**不能**用 `--only tools`：没被选中的节在阶梯上先命中 `not-selected`，
/// 所以要让"快照里没有它"露出来，必须让这一节**被选中**。
#[test]
fn a_section_the_snapshot_lacks_is_skipped_with_a_different_note() {
    let target = load("path-only");
    let local = load("machine-a");
    let p = plan(&target, &local, &all());

    for id in [SectionId::Tools, SectionId::Env, SectionId::Wsl] {
        assert_eq!(section(&p, id).status, SectionStatus::Skipped, "{id:?}");
        assert_eq!(
            section(&p, id).note.as_deref(),
            Some(NOTE_SECTION_NOT_IN_SNAPSHOT),
            "{id:?}"
        );
        assert!(section(&p, id).actions.is_empty(), "{id:?}");
        assert!(section(&p, id).counts.rows.is_empty(), "{id:?}");
    }
    assert_ne!(
        NOTE_NOT_SELECTED, NOTE_SECTION_NOT_IN_SNAPSHOT,
        "两句'没做'不许是同一句话"
    );

    // path 在快照里、也被选中：它与本机逐字节相同，只剩本机自己那条 fix。
    assert_eq!(n(&p, SectionId::Path, "fix"), 1);
    assert_eq!(
        section(&p, SectionId::Path).note.as_deref(),
        Some(NOTE_FIX_NOT_SELECTED)
    );
    assert_eq!(p.summary.skipped, 3);
    assert_eq!(p.summary.no_change, 1);
}

// ─────────────────────────────────────────────────────────────────────────────
// 六、可重入：**重跑就是继续**（决策 155）
// ─────────────────────────────────────────────────────────────────────────────

/// 上一次 `apply` 做到一半（path/env/wsl 已落地，工具装了 3 个缺 4 个）：
/// 重跑 `plan` 必须**只列出剩下那些**，而不是从头再来一遍。
///
/// 决策 155：没有 `--resume`、没有状态文件 —— 计划是从**两侧快照的差异**算出来的，
/// 所以它天然幂等。
#[test]
fn a_half_finished_apply_plans_only_what_is_left() {
    let target = load("machine-a");
    let original = plan(&target, &load("machine-b-current"), &all());
    let resumed = plan(&target, &load("halfway-local"), &all());
    let t = SectionId::Tools;

    assert_eq!(n(&resumed, t, "installed"), 3, "cargo + git + node 22.11.0");
    assert_eq!(n(&resumed, t, "missing"), 0, "没有要下载的东西了");
    assert_eq!(n(&resumed, t, "third-party"), 2);
    assert_eq!(n(&resumed, t, "unsupported"), 2);
    assert_eq!(n(&resumed, t, "extra"), 0);
    assert_eq!(section(&resumed, t).status, SectionStatus::Unsupported);
    assert!(
        !section(&resumed, t).needs_network,
        "重跑时已经没有要下载的了"
    );
    assert_eq!(effective_total(&resumed, t), 0, "这一节一个字节都不会写");

    // path / env / wsl 三个 section 已经落地：不再有任何会写的动作。
    for id in [SectionId::Path, SectionId::Wsl] {
        assert_eq!(
            section(&resumed, id).status,
            SectionStatus::NoChange,
            "{id:?}"
        );
        assert!(section(&resumed, id).actions.is_empty(), "{id:?}");
        assert_eq!(effective_total(&resumed, id), 0, "{id:?}");
    }
    assert_eq!(
        section(&resumed, SectionId::Env).status,
        SectionStatus::NoChange,
        "机器级变量已经写好了，不再需要提权"
    );
    assert_eq!(effective_total(&resumed, SectionId::Env), 0);

    // **重跑就是继续**：剩下的动作 = 原来的动作去掉"已经做完的那一条"。
    let mut left = action_set(&original, t);
    assert!(left.remove(&("tools:git".to_owned(), "install".to_owned())));
    assert_eq!(action_set(&resumed, t), left);
    assert!(
        !section(&resumed, t)
            .actions
            .iter()
            .any(|action| action.kind == "install"),
        "已经装好的那一条不许再出现"
    );

    // 全局：剩下要做的事 ⊆ 原来要做的事，而且待办真的少了。
    for id in SectionId::ALL {
        assert!(
            action_set(&resumed, id).is_subset(&action_set(&original, id)),
            "{id:?} 重跑不许冒出新的动作"
        );
    }
    assert!(resumed.summary.manual_actions < original.summary.manual_actions);
    assert_ne!(resumed.summary.manual_actions, 0, "凭据那条待办永远在");
}

/// 同一对快照连跑两次 `plan`：输出**逐字节相同**。
///
/// 这条是"两次运行的 `--json` 必须逐字节一致"那条项目级承诺在本层的落点：
/// 节的顺序、动作的顺序、`BTreeMap` 的键序、`manual_actions` 的顺序，全都不许抖。
#[test]
fn planning_the_same_pair_twice_is_byte_identical() {
    let target = load("machine-a");
    let local = load("halfway-local");
    let first = json(&plan(&target, &local, &all()));
    let second = json(&plan(&target, &local, &all()));
    assert_eq!(first, second);

    // 节的顺序固定为 SectionId::ALL。
    let ids: Vec<SectionId> = plan(&target, &local, &all())
        .sections
        .iter()
        .map(|section| section.id)
        .collect();
    assert_eq!(ids, SectionId::ALL.to_vec());

    // 手工待办的顺序也必须稳定（同一份输入两次调用逐条相同）。
    let p = plan(&target, &local, &all());
    let q = plan(&target, &local, &all());
    assert_eq!(p.manual_actions, q.manual_actions);
}

// ─────────────────────────────────────────────────────────────────────────────
// 七、两条"不许漏出去"的硬约束（决策 152/153）
// ─────────────────────────────────────────────────────────────────────────────

/// 计划里**不许有中文**，也**不许有 ARP 占位符**。
///
/// 真机上 `ToolRow.evidence` 是中文散文（`PATH 第 1 条（process-only）里有 cargo.exe`），
/// `ToolRow.path` 可能是 `<无 InstallLocation，卸载键 {GUID}>` 这种占位符。
/// 两者都只给**人**看，进计划的入口必须是稳定 slug 的数据。
#[test]
fn no_plan_field_carries_cjk_or_the_arp_placeholder() {
    let p = plan(&load("machine-a"), &load("machine-b-current"), &all());
    let text = json(&p);

    assert!(
        text.is_ascii(),
        "成功载荷必须纯 ASCII（决策 152）—— 非 ASCII 的字节在：{:?}",
        text.chars().filter(|c| !c.is_ascii()).collect::<String>()
    );
    assert!(!text.contains('<'), "ARP 占位符的左尖括号漏进来了");
    assert!(!text.contains('>'), "ARP 占位符的右尖括号漏进来了");
    assert!(!text.contains("卸载键"), "占位符的中文漏进来了");
    assert!(
        !text.contains("PATH 第 1 条"),
        "`evidence` 的中文散文漏进来了"
    );
    assert!(!text.contains("只读采纳"), "`evidence` 的中文散文漏进来了");

    // 但**数据**必须在：名字、版本、管理器、具体原因 slug。
    assert!(text.contains("oracle-jdk"));
    assert!(text.contains("2.51.0"));
    assert!(text.contains("nvm4w"));
    assert!(text.contains("oracle-jdk-redistribution-not-permitted"));
}

/// 计划里**不许出现那个 token 的任何片段**。
///
/// 固定装置里**真的**种了一条 token 形状的假凭据（`CANARY_LEAKED_TOKEN`），
/// 所以这条断言不是"两份空串相同"：先证明它真的在固定装置里，再证明它一个片段都没进计划。
/// 查所有 8 字符窗口就足够 —— 更长的子串必然包含一个 8 字符窗口。
#[test]
fn no_plan_field_carries_any_fragment_of_the_canary_token() {
    let target = load("machine-a");
    let canary = target
        .env
        .as_ref()
        .expect("env.toml")
        .var
        .iter()
        .find(|var| var.name == "CANARY_LEAKED_TOKEN")
        .expect("故意种下的假凭据必须在固定装置里");
    assert!(canary.value_raw.len() >= 8, "太短就没有可查的窗口了");

    let text = json(&plan(&target, &load("machine-b-current"), &all()));
    for window in canary.value_raw.as_bytes().windows(8) {
        let window = std::str::from_utf8(window).expect("固定装置是 UTF-8");
        assert!(
            !text.contains(window),
            "计划里出现了凭据材料的片段：{window}"
        );
    }

    // 真厂商前缀同样一个都不许有。**拼出来，不写字面量**：本仓库被 GitHub push
    // protection（GH013）拦过一次，原因就是我们自己写的检测形状被当成了真 token。
    for prefix in [
        concat!("glpat", "-"),
        concat!("ghp", "_"),
        concat!("sk", "-"),
        concat!("AK", "IA"),
    ] {
        assert!(!text.contains(prefix), "计划里出现了 {prefix}");
    }
}
