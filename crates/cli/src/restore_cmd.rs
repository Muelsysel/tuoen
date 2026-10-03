//! `tuoen restore` 的编排（票据 #16）。
//!
//! # 顺序是刻意的
//!
//! 1. **装配**（六个后端 + 我们自己的两个根，唯一一处：`detect_ctx`）；
//! 2. **根的绝对性**：相对根会让 `owner` 与遮蔽判断**静默**变成"不是/没有"
//!    （`unwired-roots`，照 `doctor` / `path diff` 的先例）；
//! 3. **读快照**：`RestoreBundle::load` —— 错误码由 core 拥有
//!    （`snapshot-io` / `snapshot-toml` / `empty-snapshot`），这里只负责说出来；
//! 4. **本机侧**：`capture_local` 在内存里跑一次（决策 126，一个字节都不落盘）；
//! 5. **计划**：`restore::plan` 是**纯函数** —— 计划与"真做"读的是同一份计划
//!    （决策 150：`--apply` 只是"把这份计划落地"，不是"另一套逻辑"）；
//! 6. **落地**：只碰 [`sections_to_apply`] 挑出来的那几节（决策 155/161）。
//!
//! # 这一层**不做**的事
//!
//! * **不碰机器级**：机器级的改动只算不写（决策 136），逐条进 `requiresElevation`
//!   与人工待办。
//! * **不搬运凭据**：快照里记的是"需要哪个凭据"，材料从来不进快照
//!   （DPAPI 是用户+机器绑定的，搬过去也会静默失败）。
//! * **不接管第三方版本管理器**：nvm4w / uv 这类，改它的符号链接要管理员，
//!   而且两个管理器会抢同一个 reparse point。
//! * **不调用 `setx`**：1024 字符处静默裁剪 + 永久展开 `%VAR%`。
//! * **不自动提权**：需要提权的部分只报告。

use std::path::Path;

use tuoen_core::capture::{CaptureError, Section};
use tuoen_core::pathdiff::{DiffClass, PathDiffOptions, Selection};
use tuoen_core::restore::{
    PlannedAction, RestoreBundle, RestoreError, RestoreOptions, RestorePlan, SectionId,
    SectionPlan, SectionStatus, plan,
};
use tuoen_platform::{
    EnvScope, FileSystem, RealFileSystem, RegHive, RegType, RegValue, Registry, USER_ENV_SUBKEY,
    apply_rewrite, plan_rewrite, write_type_for,
};

use crate::detect_ctx::Backends;
use crate::doctor_view::UnwiredRootsView;
use crate::envelope::Envelope;
use crate::exit;
use crate::manage::InstallArgs;
use crate::path_cmd::PathCliError;
use crate::path_diff_cmd::{Username, broadcast, capture_local, user_after, username};
use crate::restore::RestoreArgs;
use crate::restore_view::{
    ApplyFailure, ApplyReport, RestoreDataView, SectionOutcome, SummaryView, print_plan_human,
};

/// 装配失败的错误码（与 `doctor` / `path diff` 逐字相同：同一个失败只该有一个名字）。
const UNWIRED_ROOTS: &str = "unwired-roots";

/// 平台的 `windows-x64` recipe 面。快照里记的是"哪个平台的哪个版本"，
/// 而 restore 只在**本机**跑，所以平台是当前这一个。
const PLATFORM: &str = "windows-x64";

/// 快照里有、而 L1 的 `restore` **不还原**的 section（票据 #17）。
///
/// # 判据是"两个枚举对不上的那一半"，不是一张写死的名单
///
/// `capture::Section` 回答的是"**快照里有什么**"，`restore::SectionId` 回答的是
/// "**restore 能做什么**" —— 两个不同的问句，两套枚举（决策 165 与
/// `restore-core` 的裁定）。于是"我不还原的 section"就是**前者有、后者没有**的那些，
/// 而第七个 section 出现时这里会自动跟上，不用改一行。
///
/// 还要再看一眼磁盘：`Section::ALL` 是"**可能**有哪些"，而这一份快照真的带了它吗 ——
/// `RestoreBundle::load` 只找它自己那张表（四个 section + `skipped.toml`），
/// **忽略**不认识的 `.toml`，所以"带了 `globals.toml`"这件事在 core 的类型里看不见。
/// 这正是决策要的"看见了但不还原"，而不是"没看见"。
///
/// 用 `FileSystem::inspect` 而不是 `std::fs`：与 core 读快照走同一条路径，
/// 行为（不存在的路径不报错）一致。
fn unrestorable_sections(dir: &Path) -> Vec<String> {
    let fs = RealFileSystem;
    Section::ALL
        .iter()
        .filter(|section| SectionId::parse(section.as_str()).is_none())
        .filter(|section| fs.inspect(&dir.join(section.file_name())).exists)
        .map(|section| section.as_str().to_owned())
        .collect()
}

/// 跑 `tuoen restore`。返回退出码。
pub fn run(args: &RestoreArgs) -> i32 {
    let command = command_name(args.apply);
    let backends = Backends::assemble();
    if !backends.roots_are_absolute() {
        return report_unwired(args.json, &backends, command);
    }

    // `load` 自己就用 `empty-snapshot` 拒绝了"一个 section 文件都没有"的目录
    // （决策 156）—— 这里**不重判一次**：同一个判断有两份实现，就会有一天只改了一处。
    let target = match RestoreBundle::load(&args.dir, &RealFileSystem) {
        Ok(bundle) => bundle,
        Err(error) => return report_snapshot_failure(args.json, command, &error),
    };

    let who = username(&backends);
    let options = RestoreOptions {
        sections: args.only.clone(),
        with_fix: args.with_fix,
        current_username: who.value.clone(),
    };
    // "哪几节"只有一处定义（core 的 `effective_sections`：空 = 全部四节、顺序固定）。
    let selected = options.effective_sections();

    let ctx = backends.context(true);
    let captured = match capture_local(&backends, &ctx, capture_sections(&selected)) {
        Ok(bundle) => bundle,
        Err(error) => return report_capture_failure(args.json, command, &error),
    };
    let local = RestoreBundle::from_capture(&captured);

    // **纯函数**：计划与真做读的是同一份东西（决策 150）。
    let plan = plan(&target, &local, &options);
    // 快照里那些"我不还原"的 section（决策 183）—— 它们在 core 的类型里看不见，
    // 所以必须在计划之外单独看一眼快照目录。
    let unrestorable = unrestorable_sections(&args.dir);

    if !args.apply {
        // 默认形态与 `--dry-run` 走的是**同一条**路径、同一份计划 ——
        // 逐字节相同不是"顺手做到的"，而是因为它们本来就是同一次调用。
        return finish(args.json, &plan, &who, &unrestorable, None);
    }

    let report = apply_plan(&backends, &plan, &target, &local, &options);
    let code = if report.failed() {
        exit::RUNTIME_ERROR
    } else {
        exit::SUCCESS
    };
    finish(args.json, &plan, &who, &unrestorable, Some(&report));
    code
}

/// 计划做好了：输出 + 退出码。两种模式共用，所以"计划"这一段不可能漂移。
fn finish(
    json: bool,
    plan: &RestorePlan,
    who: &Username,
    unrestorable: &[String],
    apply: Option<&ApplyReport>,
) -> i32 {
    if json {
        // 四个顶层键**逐字来自** core 的 `RestorePlan`（引用，不重算）——
        // 摊开写只是为了在 `summary` 里塞进那个 CLI 侧拥有的 `unrestorable`。
        let view = RestoreDataView {
            snapshot: &plan.snapshot,
            sections: &plan.sections,
            manual_actions: &plan.manual_actions,
            summary: SummaryView {
                summary: &plan.summary,
                unrestorable: unrestorable.to_vec(),
            },
            apply,
        };
        crate::print_json(&Envelope::ok(command_name(apply.is_some()), &view));
    } else {
        print_plan_human(plan, who, unrestorable, apply);
    }
    exit::SUCCESS
}

fn command_name(applying: bool) -> &'static str {
    if applying {
        "restore.apply"
    } else {
        "restore.plan"
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 选择
// ─────────────────────────────────────────────────────────────────────────────

/// 采集本机侧要哪几段。**只采集被选中的**：没选中的节连读都不读
/// （读一遍再丢掉只会让"我没碰它"这句话变弱）。
fn capture_sections(selected: &[SectionId]) -> Vec<Section> {
    selected.iter().map(|id| section_of(*id)).collect()
}

fn section_of(id: SectionId) -> Section {
    match id {
        SectionId::Tools => Section::Tools,
        SectionId::Path => Section::Path,
        SectionId::Env => Section::Env,
        SectionId::Wsl => Section::Wsl,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 落地
// ─────────────────────────────────────────────────────────────────────────────

/// **这一次 apply 到底会碰哪几节** —— 决策 161 的三个"有变更"状态。
///
/// 为什么它是一个独立的纯函数而不是散在 `match` 里：票据点名要"`--only path`
/// 时 `tools` / `env` 未被改动"这条证据，而在进程边界之外"未被改动"没法直接
/// 证明。于是把"会碰谁"变成一个**可以单元测试**的判据：`Unsupported` /
/// `NoChange` / `Skipped` 一律不进这个列表，所以"没被选中"与"没有变更"的节
/// **根本走不到任何写代码**（决策 155）。
pub(crate) fn sections_to_apply(plan: &RestorePlan) -> Vec<SectionId> {
    plan.sections
        .iter()
        .filter(|section| {
            // 变异测试：把这一行换成 `true` 会让三条单测同时红 —— 它们真的在看结论。
            matches!(
                section.status,
                SectionStatus::WouldChange
                    | SectionStatus::RequiresElevation
                    | SectionStatus::NeedsNetwork
            )
        })
        .map(|section| section.id)
        .collect()
}

fn apply_plan(
    backends: &Backends,
    plan: &RestorePlan,
    target: &RestoreBundle,
    local: &RestoreBundle,
    options: &RestoreOptions,
) -> ApplyReport {
    let mut report = ApplyReport::new();
    // **只**按这个函数的结论决定碰哪些节。
    for id in sections_to_apply(plan) {
        let Some(section) = plan.section(id) else {
            continue;
        };
        let applied = match id {
            SectionId::Tools => apply_tools(section),
            SectionId::Path => apply_path(backends, section, target, local, options),
            SectionId::Env => apply_env(backends, section, target),
            // WSL 只报告：装发行版要 `wsl --install`（会重启、会改 Windows 功能），
            // 那不是"还原一份环境快照"该顺手做的事。
            SectionId::Wsl => SectionApply::plain(section, "report-only", false),
        };
        report.push(applied.outcome, applied.failures);
    }
    report
}

/// 一个 section 的落地结果 + 它带出来的**全部**失败。
struct SectionApply {
    outcome: SectionOutcome,
    failures: Vec<ApplyFailure>,
}

impl SectionApply {
    fn plain(section: &SectionPlan, outcome: &'static str, wrote: bool) -> Self {
        Self {
            outcome: SectionOutcome {
                id: section.id.as_str(),
                outcome,
                wrote,
                broadcast_replies: None,
                error: None,
            },
            failures: Vec::new(),
        }
    }

    fn failed(section: &SectionPlan, code: &'static str, detail: String) -> Self {
        let failure = ApplyFailure { code, detail };
        Self {
            outcome: SectionOutcome {
                id: section.id.as_str(),
                outcome: "failed",
                wrote: false,
                broadcast_replies: None,
                error: Some(failure.clone()),
            },
            failures: vec![failure],
        }
    }
}

/// `tools`：缺失且可复现的行走 L0 的安装流（决策 152）。
///
/// 每一行都是一次 `install`，而 `install` 自己就是原子的（staging → 提交或回滚，
/// 中途失败不留半个目录）。一行失败不挡下一行：能装的照装，装不了的逐条报出来，
/// 退出码是 1（"说清哪些没做"）。
fn apply_tools(section: &SectionPlan) -> SectionApply {
    let mut wrote = false;
    let mut failures = Vec::new();
    for action in &section.actions {
        if action.kind != "install" {
            continue;
        }
        let spec = spec_of(action);
        let args = InstallArgs {
            spec: spec.clone(),
            platform: PLATFORM.to_owned(),
            r#use: false,
            dry_run: false,
            json: false,
        };
        match crate::manage_cmd::install(&args) {
            Ok(_) => wrote = true,
            Err(error) => failures.push(ApplyFailure {
                code: error.code,
                detail: spec,
            }),
        }
    }
    section_apply(section, wrote, failures)
}

/// 从计划里那一条 `install` 反推出 `install` 命令要的 `spec`。
///
/// `detail` 的形状由决策 152 钉死：`recipe=nodejs version=24.19.0 manager=nvm4w`。
/// 取 `recipe=` 而不是 `subject`：`subject` 是给人看的名字，而 `spec` 要能被
/// 目录解析（别名与显示名都可能与 id 不同）。
fn spec_of(action: &PlannedAction) -> String {
    let mut tool = action.subject.clone();
    let mut version = None;
    for field in action.detail.split_whitespace() {
        if let Some(value) = field.strip_prefix("recipe=") {
            tool = value.to_owned();
        }
        if let Some(value) = field.strip_prefix("version=") {
            version = Some(value.to_owned());
        }
    }
    match version {
        Some(version) => format!("{tool}@{version}"),
        None => tool,
    }
}

/// `path`：复用 #15 的 `diff` + `rebuild` + L0 的 `plan_rewrite` / `apply_rewrite`。
///
/// 选中的类**不重新推导**：计划里 `actions[].kind` 就是六类 slug，把它们收回来
/// 交给 `Selection` —— "什么算改动"只有 `tuoen_core::pathdiff` 一处定义。
fn apply_path(
    backends: &Backends,
    section: &SectionPlan,
    target: &RestoreBundle,
    local: &RestoreBundle,
    options: &RestoreOptions,
) -> SectionApply {
    let (Some(local_path), Some(target_path)) = (&local.path, &target.path) else {
        return SectionApply::plain(section, "skipped", false);
    };
    let classes = selected_classes(section);
    if classes.is_empty() {
        return SectionApply::plain(section, "no-change", false);
    }

    let result = tuoen_core::pathdiff::diff(
        local_path,
        target_path,
        &RealFileSystem,
        &PathDiffOptions {
            current_username: options.current_username.as_deref(),
        },
    );
    let selection = Selection::new(classes, Vec::new());
    let rebuilt = tuoen_core::pathdiff::rebuild(local_path, target_path, &result, &selection);

    // 悬崖守卫（决策 140）：超过 8191 之后 `cmd.exe` **完全忽略整条 `PATH`**。
    // `path apply` 在 `--dry-run` 时不拒绝（那是唯一能给出重建后完整列表的地方），
    // 但 `restore --apply` 是**真写**，所以这里必须拒绝。
    if rebuilt.budget.level == tuoen_platform::BudgetLevel::Exceeded.as_str() {
        let over = rebuilt
            .budget
            .effective_chars
            .saturating_sub(rebuilt.budget.cliff);
        return SectionApply::failed(
            section,
            "too-long",
            format!(
                "effective={} cliff={} over={over}",
                rebuilt.budget.effective_chars, rebuilt.budget.cliff
            ),
        );
    }

    let user_after = user_after(&rebuilt);
    let path_plan = match plan_rewrite(backends.env(), EnvScope::User, &user_after) {
        Ok(path_plan) => path_plan,
        Err(error) => {
            return SectionApply::failed(
                section,
                crate::path_cmd::platform_error_code(&error),
                "plan-rewrite".to_owned(),
            );
        }
    };
    if !path_plan.will_write() {
        // 计划与现状一致：**不写、不广播**（决策 155）。
        return SectionApply::plain(section, "no-change", false);
    }

    match apply_rewrite(backends.registry(), &path_plan, broadcast) {
        Ok(applied) => {
            let mut apply = SectionApply::plain(section, "applied", true);
            apply.outcome.broadcast_replies = Some(applied.broadcast_replies);
            apply
        }
        Err(error) => SectionApply::failed(
            section,
            crate::path_cmd::platform_error_code(&error),
            format!("chars={}", path_plan.after_raw.chars().count()),
        ),
    }
}

/// 计划里这一节选中了哪几类（去重、保持计划里的顺序）。
fn selected_classes(section: &SectionPlan) -> Vec<DiffClass> {
    let mut classes = Vec::new();
    for action in &section.actions {
        if let Some(class) = DiffClass::parse(&action.kind)
            && !classes.contains(&class)
        {
            classes.push(class);
        }
    }
    classes
}

/// `env`：用户级缺失的整值写 + **一次**广播；机器级不写（决策 136）。
fn apply_env(backends: &Backends, section: &SectionPlan, target: &RestoreBundle) -> SectionApply {
    let Some(env_file) = &target.env else {
        return SectionApply::plain(section, "skipped", false);
    };
    let mut wrote = false;
    let mut failures = Vec::new();
    for action in &section.actions {
        match action.kind.as_str() {
            "set-user" => {
                let Some(row) = env_file.var.iter().find(|row| {
                    row.name.eq_ignore_ascii_case(&action.subject) && row.scope == EnvScope::User
                }) else {
                    failures.push(ApplyFailure {
                        code: "env-row-missing",
                        detail: action.subject.clone(),
                    });
                    continue;
                };
                // 类型规则只有一处（`write_type_for`）：值含 `%` 才 `REG_EXPAND_SZ`，
                // 否则保留原有类型。`setx` 的两种事故都出在这条规则上。
                let value = match write_type_for(&row.value_raw, Some(row.reg_type)) {
                    RegType::Sz => RegValue::Sz(row.value_raw.clone()),
                    RegType::ExpandSz => RegValue::ExpandSz(row.value_raw.clone()),
                };
                // `RegHive::Hkcu` 的 `resolve()` 认识**裸的 `Environment`**
                // （`ABSOLUTE_EXACT`）—— 它落点是 `HKCU\Environment`，
                // **不是** `HKCU\SOFTWARE\Environment`。这正是 `apply_rewrite`
                // 写用户级 `PATH` 用的那条路径，所以这里不另开一条低层通道。
                match backends.registry().set_value(
                    RegHive::Hkcu,
                    USER_ENV_SUBKEY,
                    &row.name,
                    &value,
                ) {
                    Ok(()) => wrote = true,
                    Err(error) => failures.push(ApplyFailure {
                        code: crate::path_cmd::platform_error_code(&error),
                        detail: row.name.clone(),
                    }),
                }
            }
            // 机器级要提权，而 tuoen 不做静默提权（决策 12/136）：只报告。
            "set-machine" => {}
            _ => {}
        }
    }
    let mut apply = section_apply(section, wrote, failures);
    if wrote {
        // **一次**广播覆盖这一节的全部写入：`WM_SETTINGCHANGE` 是"环境变了"的
        // 一次性通知，按变量发 N 次只会让 N 个窗口各刷 N 遍。
        apply.outcome.broadcast_replies = Some(broadcast());
    }
    apply
}

/// 三个"落地型" section 共用的收尾：有没有写、失败了几条、退出码怎么读。
fn section_apply(section: &SectionPlan, wrote: bool, failures: Vec<ApplyFailure>) -> SectionApply {
    let outcome = if failures.is_empty() {
        if wrote { "applied" } else { "no-change" }
    } else {
        "failed"
    };
    SectionApply {
        outcome: SectionOutcome {
            id: section.id.as_str(),
            outcome,
            wrote,
            broadcast_replies: None,
            error: failures.first().cloned(),
        },
        failures,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 失败路径
// ─────────────────────────────────────────────────────────────────────────────

fn report_failure(json: bool, command: &'static str, error: &PathCliError) -> i32 {
    if json {
        crate::print_json(&Envelope::err(command, error.code, error.message.clone()));
    } else {
        eprintln!("tuoen: {}", error.message);
    }
    exit::RUNTIME_ERROR
}

/// 装配失败：算不出我们自己的根位置（两个根里有不是绝对路径的）。
fn report_unwired(json: bool, backends: &Backends, command: &'static str) -> i32 {
    let message = format!(
        "算不出 tuoen 自己的根位置：`%LOCALAPPDATA%` 与 `%USERPROFILE%` 都读不到，\
         于是存储根退成了相对路径 `{}`。\n\
         相对的根本分不出「这条 PATH 条目是不是我们自己的」，所以这一次**不做计划**。",
        backends.store_root().display()
    );
    let data = UnwiredRootsView {
        store_root: backends.store_root().display().to_string(),
        shim_dir: backends.shim_dir().display().to_string(),
    };
    if json {
        crate::print_json(&Envelope::partial(command, UNWIRED_ROOTS, message, &data));
    } else {
        eprintln!("tuoen: {message}");
    }
    exit::RUNTIME_ERROR
}

/// 读快照失败。**三个错误码必须分得开**（core 的 `RestoreError`）：文件不在 /
/// 内容变了 / 里面一个 section 都没有 —— 用户要做的下一步完全不同。
fn report_snapshot_failure(json: bool, command: &'static str, error: &RestoreError) -> i32 {
    if json {
        crate::print_json(&Envelope::err(command, error.code(), error.to_string()));
    } else {
        eprintln!("tuoen: {error}");
    }
    exit::RUNTIME_ERROR
}

/// 空快照由 core 的 `RestoreBundle::load` 拒绝（`empty-snapshot`，决策 156），
/// 这里只需要把它的错误原样报出来 —— 见 [`report_snapshot_failure`]。
fn report_capture_failure(json: bool, command: &'static str, error: &CaptureError) -> i32 {
    report_failure(
        json,
        command,
        &PathCliError {
            code: error.code(),
            message: format!("采集本机状态失败：{error}"),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_core::restore::{RestoreSummary, SectionCounts};

    fn section(id: SectionId, status: SectionStatus) -> SectionPlan {
        SectionPlan {
            id,
            status,
            counts: SectionCounts::default(),
            actions: Vec::new(),
            needs_network: false,
            requires_elevation: false,
            note: None,
        }
    }

    fn plan_of(sections: Vec<SectionPlan>) -> RestorePlan {
        RestorePlan {
            snapshot: "tuoen.d".to_owned(),
            sections,
            manual_actions: Vec::new(),
            summary: RestoreSummary::default(),
        }
    }

    #[test]
    fn only_path_touches_only_path() {
        // 票据点名的证据：`--only path` 时另外三节是 `skipped`，所以它们
        // **根本走不到任何写代码**。
        let plan = plan_of(vec![
            section(SectionId::Tools, SectionStatus::Skipped),
            section(SectionId::Path, SectionStatus::WouldChange),
            section(SectionId::Env, SectionStatus::Skipped),
            section(SectionId::Wsl, SectionStatus::Skipped),
        ]);
        assert_eq!(sections_to_apply(&plan), vec![SectionId::Path]);
    }

    #[test]
    fn an_empty_plan_touches_nothing() {
        // 全 `no-change` 的真机形态（决策 155：空计划不许走到任何写代码）。
        let plan = plan_of(vec![
            section(SectionId::Tools, SectionStatus::NoChange),
            section(SectionId::Path, SectionStatus::NoChange),
            section(SectionId::Env, SectionStatus::NoChange),
            section(SectionId::Wsl, SectionStatus::NoChange),
        ]);
        assert!(sections_to_apply(&plan).is_empty());
    }

    #[test]
    fn the_three_change_statuses_all_count() {
        let plan = plan_of(vec![
            section(SectionId::Tools, SectionStatus::NeedsNetwork),
            section(SectionId::Path, SectionStatus::RequiresElevation),
            section(SectionId::Env, SectionStatus::WouldChange),
            section(SectionId::Wsl, SectionStatus::Unsupported),
        ]);
        assert_eq!(
            sections_to_apply(&plan),
            vec![SectionId::Tools, SectionId::Path, SectionId::Env],
            "Unsupported 不算「有变更」"
        );
    }

    #[test]
    fn an_empty_only_means_all_four_sections() {
        // "空 = 我没限制"这条约定由 core 的 `effective_sections` 拥有，
        // CLI 只是把空 `--only` 原样传下去 —— 这里钉住"传下去之后是四节"。
        let options = RestoreOptions::default();
        assert_eq!(options.effective_sections(), SectionId::ALL.to_vec());
        assert_eq!(capture_sections(&options.effective_sections()).len(), 4);
    }

    #[test]
    fn the_spec_comes_from_the_recipe_field() {
        // `subject` 是给人看的名字，`spec` 要能被目录解析 —— 取 `recipe=`。
        let action = PlannedAction {
            id: "nodejs@24.19.0".to_owned(),
            kind: "install".to_owned(),
            subject: "Node.js".to_owned(),
            detail: "recipe=nodejs version=24.19.0 manager=nvm4w".to_owned(),
        };
        assert_eq!(spec_of(&action), "nodejs@24.19.0");
    }

    #[test]
    fn a_plan_without_a_version_still_gives_a_spec() {
        let action = PlannedAction {
            id: "nodejs".to_owned(),
            kind: "install".to_owned(),
            subject: "nodejs".to_owned(),
            detail: "recipe=nodejs".to_owned(),
        };
        assert_eq!(spec_of(&action), "nodejs");
    }

    #[test]
    fn classes_come_from_the_plan_and_are_deduplicated() {
        let mut path = section(SectionId::Path, SectionStatus::WouldChange);
        path.actions = vec![
            action("user:+1", "add"),
            action("user:+2", "add"),
            action("user:3", "fix"),
        ];
        assert_eq!(
            selected_classes(&path),
            vec![DiffClass::Add, DiffClass::Fix],
            "重复的类只留一次，顺序按计划"
        );
    }

    fn action(id: &str, kind: &str) -> PlannedAction {
        PlannedAction {
            id: id.to_owned(),
            kind: kind.to_owned(),
            subject: "C:\\x".to_owned(),
            detail: "toIndex=1".to_owned(),
        }
    }
}
