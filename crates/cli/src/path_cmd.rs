//! `tuoen path show` / `add` / `remove` 的**执行**部分。
//!
//! 参数形状在 [`crate::path`]，`--json` 形状与人类输出在 [`crate::path_view`]；
//! 这里只做编排：把"看/改用户级 `PATH`"这件事交给平台层已经实现并测过的那套
//! `analyze` / `plan_*` / `apply`。
//!
//! ## 三条贯穿这一族的决定
//!
//! 1. **这一层不重写任何 `PATH` 逻辑。** 解析、比较、预算、遮蔽全是
//!    `tuoen_platform::path` 的，连"改动的形状"（`PathChange`）也是直接复用的 ——
//!    在这里再定义一遍计划类型，只会多出一份必然会漂移的副本。
//! 2. **写只在 `PathPlan::will_write()` 为真时发生**，而 `apply` 自己就守着这一条
//!    （它不写的时候连广播都不发）。所以这里**不做第二套判断**：多一层"我觉得该写"
//!    的判断就是第二个事实来源。
//! 3. **拒绝与空操作是两种结果。** 拿 shim 目录去 `remove` 会被**拒绝**（退出码非 0），
//!    而"本来就不在里面"是**成功的空操作**（退出码 0，并且明说没有改任何东西）。
//!    把两者合并成一种，会让脚本分不清"我不给你摘 shim 目录"与"那里本来就没有它"。
//!
//! ## 这一层**不做**的事
//!
//! * **不碰机器级 `PATH`**：那要提权，而 tuoen 不做"顺手提权"（见设计决策）。
//! * **不调用 `setx`**：1024 字符处静默裁剪 + 永久展开 `%VAR%`。
//! * **不自动清理**重复、失效、硬编码用户名的条目（决策 24）：`show` 只报告它们。
//! * **不加"用假注册表"的环境变量后门**：一个被误设的变量会把垃圾写进用户真实的
//!   `PATH`。测试要隔离的是**存储位置**（`LOCALAPPDATA`），而不是注册表。

use std::path::{Path, PathBuf};

use tuoen_platform::{
    PathAnalysis, PathApplied, PathPlan, PlatformError, RealEnvBlock, RealFileSystem,
    RealProcessEnv, RealRegistry,
};
use tuoen_store::Store;

use crate::envelope::Envelope;
use crate::exit;
use crate::path::{PathAddArgs, PathRemoveArgs, PathShowArgs};
use crate::path_view::{PathPlanView, PathShowView, print_plan_human, print_show_human};

/// 编排层的一条错误。**错误码稳定、消息中文**，两者分开的理由与 `envelope` 那边一样。
#[derive(Debug)]
pub struct PathCliError {
    /// 稳定的小写 kebab ASCII 错误码（进 `--json`）。
    pub code: &'static str,
    /// 中文消息。
    pub message: String,
}

impl PathCliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for PathCliError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// 平台错误 → 稳定 slug。**看的是种类，不是消息文本**
/// （本机 zh-CN，`Display` 出来的是中文，按它分支换台机器就静默失效）。
pub fn platform_error_code(error: &PlatformError) -> &'static str {
    match error {
        PlatformError::Unsupported { .. } => "unsupported",
        PlatformError::AccessDenied { .. } => "access-denied",
        PlatformError::Win32 { .. } => "win32",
        // `RegistryKeyMissing` 到不了这里（"读不到"被读成"这个作用域没有 Path"），
        // 但真到了也不该被报成别的种类。
        PlatformError::RegistryKeyMissing { .. } => "registry-key-missing",
    }
}

/// 平台错误 → 中文消息。`;` 这一条要额外解释，因为**它看起来应该能用引号解决**。
fn platform_error_message(error: &PlatformError, dir: &str) -> String {
    if let PlatformError::Unsupported { .. } = error {
        return format!(
            "不能把 `{dir}` 当成一条 `PATH` 条目：它里面含 `;`。\n\
             `;` 是 `PATH` 的**条目分隔符**，而引号保护不了它 —— \
             Windows 先把整条值按 `;` 切开，再决定要不要剥掉引号，\
             所以 `C:\\a;b` 会被写成两条独立的条目。\n\
             那不是你想要的事，而一旦写进去，我们再也分不出它原本是一条还是两条。\
             所以宁可当场拒绝：请改用不含 `;` 的目录名。"
        );
    }
    format!("{error}")
}

// ─────────────────────────────────────────────────────────────────────────────
// show
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen path show`（也是不带子命令的 `tuoen path`）。**只读，永远退出码 0。**
pub fn run_show(args: &PathShowArgs) -> i32 {
    let (view, _analysis) = show(args);
    if args.json {
        crate::print_json(&Envelope::ok("path.show", &view));
    } else {
        print_show_human(&view);
    }
    exit::SUCCESS
}

/// `show` 的全部逻辑，与输出方式无关 —— 这样测试能直接看返回值。
///
/// 返回 `(视图, 原始分析)`：视图是给 `--json` / 人类输出的，原始分析留给
/// 进程内的测试（它带着**平台层的**类型，而视图已经把它们拍平成了稳定形状）。
/// 两者来自**同一次** `analyze` —— 算两遍会出现"报告里是这一次、返回的是那一次"。
#[must_use]
pub fn show(args: &PathShowArgs) -> (PathShowView, PathAnalysis) {
    let _ = args;
    let analysis = analysis();
    let shim = shim_dir().map(|dir| dir.display().to_string());
    let view = PathShowView::new(&analysis, shim.as_deref());
    (view, analysis)
}

/// 一次只读分析。`shim_dir` 给了就顺带做遮蔽检测。
#[must_use]
pub fn analysis() -> PathAnalysis {
    let block = RealEnvBlock::new(RealRegistry, RealFileSystem);
    let process = RealProcessEnv;
    tuoen_platform::analyze(&block, &process, &RealFileSystem, shim_dir().as_deref())
}

/// shim 目录：**全仓唯一的那一处定义**在 [`crate::shim_cmd`]，这里只转发。
///
/// 顺手扩成 `pub(crate)` 而不是在本地再拼一遍 `<家目录>\shims`：第二份定义
/// 会漂移，而它漂移的后果是"遮蔽检测看的是一个不存在的目录"——
/// 报告里会静默地少掉一整类发现。
#[must_use]
pub fn shim_dir() -> Option<PathBuf> {
    Some(crate::shim_cmd::shim_dir(&Store::at_default_location()))
}

// ─────────────────────────────────────────────────────────────────────────────
// add / remove
// ─────────────────────────────────────────────────────────────────────────────

/// 一次改动请求的结果。**拒绝与空操作是两种结果**，见模块文档。
#[derive(Debug)]
pub enum PathOutcome {
    /// 计划做完了（真的写了，或者计划本身就是空操作）。
    Planned(Box<PathPlanView>),
    /// 计划**拒绝**执行（现在只有一种：不许删 shim 目录）。
    Refused {
        code: &'static str,
        message: String,
        plan: Box<PathPlanView>,
    },
}

impl PathOutcome {
    /// 落盘结果与计划一起交给视图层 —— 两者必须来自同一次调用，
    /// 否则报告里会出现"计划是这个、写的是那个"。
    fn planned(
        action: &'static str,
        dir: &str,
        plan: &PathPlan,
        dry_run: bool,
        applied: Option<&PathApplied>,
    ) -> Self {
        Self::Planned(Box::new(PathPlanView::new(
            action, dir, plan, dry_run, applied,
        )))
    }

    /// 补上"做完这一步之后重新分析"的遮蔽现状。**拒绝**没有这一步
    /// （什么都不写，现状不会变，也就没什么可复核的）。
    fn with_shadowing(self, analysis: &PathAnalysis, shim_dir: Option<&Path>) -> Self {
        match self {
            Self::Planned(view) => Self::Planned(Box::new(view.with_shadowing(analysis, shim_dir))),
            refused => refused,
        }
    }
}

/// 跑 `tuoen path add`。返回退出码。
pub fn run_add(args: &PathAddArgs) -> i32 {
    finish(add(args), args.json)
}

/// 跑 `tuoen path remove`。返回退出码。
pub fn run_remove(args: &PathRemoveArgs) -> i32 {
    finish(remove(args), args.json)
}

/// 成败 → 输出 → 退出码。两条写命令共用，所以它们的输出契约不可能漂移。
fn finish(outcome: Result<PathOutcome, PathCliError>, json: bool) -> i32 {
    match outcome {
        Ok(PathOutcome::Planned(view)) => {
            if json {
                crate::print_json(&Envelope::ok("path.plan", &view));
            } else {
                print_plan_human(&view);
            }
            exit::SUCCESS
        }
        Ok(PathOutcome::Refused {
            code,
            message,
            plan,
        }) => {
            // **拒绝也带 `data`**：计划里写着"你给的那个目录是 shim 目录"，
            // 而那正是拒绝的理由。纯失败信封会让消费者看不到这个理由。
            if json {
                crate::print_json(&Envelope::partial("path.plan", code, message, &plan));
            } else {
                print_plan_human(&plan);
                eprintln!("tuoen: {message}");
            }
            exit::RUNTIME_ERROR
        }
        Err(error) => {
            if json {
                crate::print_json(&Envelope::err("path.plan", error.code, error.message));
            } else {
                eprintln!("tuoen: {}", error.message);
            }
            exit::RUNTIME_ERROR
        }
    }
}

/// `add` 的全部逻辑，与输出方式无关。
///
/// # Errors
///
/// `dir` 里含 `;` 时返回 `unsupported` —— 那会被写成两条条目，见
/// [`platform_error_message`]。
pub fn add(args: &PathAddArgs) -> Result<PathOutcome, PathCliError> {
    let dir = args.dir.as_str();
    let block = RealEnvBlock::new(RealRegistry, RealFileSystem);
    let plan = tuoen_platform::plan_add(&block, dir).map_err(|error| {
        PathCliError::new(
            platform_error_code(&error),
            platform_error_message(&error, dir),
        )
    })?;

    // `add` 没有拒绝的分支：追加永远不会遮蔽已有的条目（它加在末尾），
    // 所以"已经在里面了"就是这里唯一的空操作 —— 那是**成功**，不是拒绝。
    if args.dry_run {
        // 演练走的是**同一条**计划代码 —— 这里只是不落盘。
        return Ok(PathOutcome::planned("add", dir, &plan, true, None)
            .with_shadowing(&analysis(), shim_dir().as_deref()));
    }
    let applied = tuoen_platform::apply(&RealRegistry, &plan, broadcast).map_err(|error| {
        PathCliError::new(
            platform_error_code(&error),
            platform_error_message(&error, dir),
        )
    })?;
    Ok(
        PathOutcome::planned("add", dir, &plan, false, Some(&applied)).with_shadowing(
            // **落盘之后重新分析**：注册表在这一刻已经变了，拿计划之前那次分析
            // 就是在报告一个已经不存在的世界（决策 73）。
            &analysis(),
            shim_dir().as_deref(),
        ),
    )
}

/// `remove` 的全部逻辑，与输出方式无关。
///
/// # Errors
///
/// 同 [`add`]。
pub fn remove(args: &PathRemoveArgs) -> Result<PathOutcome, PathCliError> {
    let dir = args.dir.as_str();
    let block = RealEnvBlock::new(RealRegistry, RealFileSystem);
    let shim_dir = shim_dir();
    let plan = tuoen_platform::plan_remove(&block, dir, shim_dir.as_deref()).map_err(|error| {
        PathCliError::new(
            platform_error_code(&error),
            platform_error_message(&error, dir),
        )
    })?;

    // **拒绝先于演练判断**：`--dry-run` 时不许删的东西，真跑时同样不许删 ——
    // 演练报"可以删"而真跑拒绝，是这个项目最不该出现的一类不一致。
    if let Some(code) = refusal(&plan) {
        let message = refusal_message(&plan, dir);
        return Ok(PathOutcome::Refused {
            code,
            message,
            plan: Box::new(PathPlanView::new("remove", dir, &plan, args.dry_run, None)),
        });
    }

    if args.dry_run {
        return Ok(PathOutcome::planned("remove", dir, &plan, true, None)
            .with_shadowing(&analysis(), shim_dir.as_deref()));
    }
    let applied = tuoen_platform::apply(&RealRegistry, &plan, broadcast).map_err(|error| {
        PathCliError::new(
            platform_error_code(&error),
            platform_error_message(&error, dir),
        )
    })?;
    Ok(
        PathOutcome::planned("remove", dir, &plan, false, Some(&applied))
            .with_shadowing(&analysis(), shim_dir.as_deref()),
    )
}

/// 计划里有没有一条**拒绝**。
fn refusal(plan: &PathPlan) -> Option<&'static str> {
    plan.changes.iter().find_map(crate::path_view::noop_refusal)
}

/// 拒绝时进信封 `error.message` 的中文说明。
fn refusal_message(plan: &PathPlan, dir: &str) -> String {
    let reason = plan
        .changes
        .iter()
        .find_map(|change| match change {
            tuoen_platform::PathChange::Noop { reason, .. } => Some(*reason),
            _ => None,
        })
        .unwrap_or(tuoen_platform::NoopReason::Absent);
    crate::path_view::noop_reason_human(reason, dir)
}

/// 广播闭包。**只在真的写了之后被调用** —— `apply` 保证这一点
/// （它不写的时候直接返回 `wrote: false`，一次广播都不发）。
fn broadcast() -> usize {
    tuoen_platform::sys::broadcast_environment_change()
}

// ─────────────────────────────────────────────────────────────────────────────
// 参数形状的分发
// ─────────────────────────────────────────────────────────────────────────────

/// 跑一次 `tuoen path …`。返回退出码。
///
/// 三个叶子命令各自带 `--json`（与 `shim` 那一族一致），所以这里只做分发 ——
/// 没有"父级与叶子谁说了算"这个问题。
pub fn run_command(command: &crate::path::PathCommand) -> i32 {
    use crate::path::PathCommand;
    match command {
        PathCommand::Show(args) => run_show(args),
        PathCommand::Add(args) => run_add(args),
        PathCommand::Remove(args) => run_remove(args),
        // `diff` / `apply` 的编排在 `path_diff_cmd`（票据 #15）：这一族的前三个命令
        // 只动一条条目，而那两个命令动的是**整条 `PATH`**，所以它们有自己的模块。
        PathCommand::Diff(args) => crate::path_diff_cmd::run_diff(args),
        PathCommand::Apply(args) => crate::path_diff_cmd::run_apply(args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_platform::{NoopReason, PathChange, RegType, ScopedPath};

    fn plan_with(changes: Vec<PathChange>) -> PathPlan {
        PathPlan {
            scope: tuoen_platform::EnvScope::User,
            before_raw: r"C:\a".to_owned(),
            after_raw: r"C:\a;C:\b".to_owned(),
            before_type: RegType::Sz,
            after_type: RegType::Sz,
            changes,
            changes_value: true,
            budget_after: tuoen_platform::PathBudget::of(0, 0),
        }
    }

    #[test]
    fn platform_errors_map_to_stable_slugs_never_to_text() {
        // 本机是 zh-CN，`Display` 出来的是中文 —— 按文本分支换台机器就静默失效。
        let unsupported = PlatformError::Unsupported {
            what: "含 `;` 的目录名".to_owned(),
        };
        assert_eq!(platform_error_code(&unsupported), "unsupported");
        let denied = PlatformError::AccessDenied {
            code: 5,
            path: r"HKCU\Environment".to_owned(),
        };
        assert_eq!(platform_error_code(&denied), "access-denied");
        for code in [
            platform_error_code(&unsupported),
            platform_error_code(&denied),
        ] {
            assert!(
                !code.is_empty()
                    && code
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{code}` 不是小写 kebab ASCII"
            );
        }
    }

    #[test]
    fn the_semicolon_refusal_explains_why_quoting_cannot_help() {
        // 这一条是**用户最可能不服气的一处**："我加个引号不就行了？"
        // —— 所以消息必须回答它，而不是只说"不支持"。
        let error = PlatformError::Unsupported {
            what: "含 `;` 的目录名".to_owned(),
        };
        let message = platform_error_message(&error, r"C:\a;b");
        assert!(message.contains(r"C:\a;b"), "要点名是哪个目录：{message}");
        assert!(message.contains(';'), "要说清 `;` 是分隔符：{message}");
        assert!(
            message.contains("引号保护不了"),
            "要正面回答「加引号行不行」：{message}"
        );
        assert!(
            message.contains("两条"),
            "要说清后果是变成两条条目：{message}"
        );
    }

    #[test]
    fn only_the_protected_shim_dir_counts_as_a_refusal() {
        assert_eq!(
            refusal(&plan_with(vec![PathChange::Add {
                value: r"C:\b".to_owned(),
                at: 1
            }])),
            None
        );
        assert_eq!(
            refusal(&plan_with(vec![PathChange::Noop {
                value: r"C:\a".to_owned(),
                reason: NoopReason::AlreadyPresent
            }])),
            None,
            "「已经在里面」是成功的空操作，不是拒绝"
        );
        assert_eq!(
            refusal(&plan_with(vec![PathChange::Noop {
                value: r"C:\a".to_owned(),
                reason: NoopReason::Absent
            }])),
            None,
            "「本来就不在」是成功的空操作，不是拒绝"
        );
        assert_eq!(
            refusal(&plan_with(vec![PathChange::Noop {
                value: r"C:\shims".to_owned(),
                reason: NoopReason::ProtectedShimDir
            }])),
            Some("protected-shim-dir")
        );
    }

    #[test]
    fn the_refusal_message_says_what_would_be_lost() {
        let plan = plan_with(vec![PathChange::Noop {
            value: r"C:\shims".to_owned(),
            reason: NoopReason::ProtectedShimDir,
        }]);
        let message = refusal_message(&plan, r"C:\shims");
        assert!(message.contains(r"C:\shims"), "{message}");
        assert!(message.contains("shim"), "{message}");
        // 拒绝要给出出路：这件事你自己手动改。
        assert!(message.contains("手动"), "{message}");
    }

    #[test]
    fn a_scope_with_no_path_at_all_is_still_reported_faithfully() {
        // 计划里只有注册表那一半的数，而 `PathPlan::budget_after` 的文档明说它是**下界**。
        // 这里钉住视图层没有把"下界"说成"精确值"。
        let plan = plan_with(Vec::new());
        let view = PathPlanView::new("add", r"C:\b", &plan, true, None);
        assert!(view.dry_run);
        assert_eq!(view.scope, "user", "只写用户级");
        assert!(view.applied.is_none(), "演练没有落盘结果");
        assert_eq!(view.before_type, "sz");
    }

    #[test]
    fn an_empty_scope_is_not_a_special_shape() {
        // `analyze` 在没有 `Path` 时给的是**空原文 + 一条空条目**，
        // 而不是"没有这个作用域"。这一条钉住我们照实报，不去猜。
        let scope = ScopedPath {
            scope: tuoen_platform::EnvScope::User,
            raw: String::new(),
            reg_type: None,
            entries: Vec::new(),
            chars: 0,
            empty_positions: Vec::new(),
        };
        let analysis = PathAnalysis {
            scopes: vec![scope],
            effective: Vec::new(),
            budget: tuoen_platform::PathBudget::of(0, 0),
            process_only: Vec::new(),
            duplicates: Vec::new(),
            dangling: Vec::new(),
            username_dependencies: Vec::new(),
            shadowed_shims: Vec::new(),
            shim_commands: Vec::new(),
        };
        let view = PathShowView::new(&analysis, None);
        assert_eq!(view.user_entries, 0);
        assert_eq!(view.scopes.len(), 1);
        assert_eq!(
            view.scopes[0].reg_type, None,
            "没有 Path 就是 null，不是猜一个"
        );
    }
}
