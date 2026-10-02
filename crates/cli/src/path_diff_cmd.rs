//! `tuoen path diff` / `tuoen path apply` 的**执行**部分。
//!
//! 参数形状在 [`crate::path_diff`]，`--json` 形状与人类输出在 [`crate::path_diff_view`]；
//! 这里只做编排 —— diff、重建、选择性应用、长度预算全是
//! `tuoen_core::pathdiff` 的**纯函数**，写回是 `tuoen_platform::plan_rewrite` /
//! `apply_rewrite`（L0 已经真机验过的那条写路径）。在这里再定义一遍"什么算改动"，
//! 只会多出一份必然会漂移的副本。
//!
//! # 两侧同一形状（决策 126）
//!
//! **本机侧**是现场 `capture` 出来的 `PathFile` —— **在内存里，不落盘**；
//! **目标侧**是读进来的 `tuoen.d/path.toml`。两侧不同形状的判据必然漂移
//! （决策 20 的同一条理由）。所以 `capture` 在这里是**一次采集**，`diff` 与
//! `rebuild` 都吃它的产物；`--dry-run` 与真写之间也没有第二次采集。
//!
//! 时间戳（`captured_at`）照样要给 `CaptureOptions`，但它只活在内存里：
//! 进 `--json` 的每一个数字都从 `PathDiff` / `Rebuild` 来，两者都没有时间戳 ——
//! 于是同一条命令跑两次**逐字节相同**。
//!
//! # 顺序是刻意的
//!
//! 1. **装配**（六个后端 + 我们自己的两个根）；
//! 2. **根的绝对性**：相对根会让 `owner` 与遮蔽判断**静默**变成"不是/没有"，
//!    所以宁可失败（`unwired-roots`，照 `doctor` 的先例）；
//! 3. **空选择**：在读注册表**之前**就拒绝（`nothing-selected`，决策 135）——
//!    这一票动的是用户整条 `PATH`，"顺手删掉几条"不许成为默认行为；
//! 4. 采集本机侧 → 读目标侧 → diff → rebuild；
//! 5. **长度预算**（决策 140 + Lead 的补充裁决）：**真写**在 `exceeded` 时拒绝
//!    （`too-long`，退出 1），而 **`--dry-run` 照常成功**并额外说明"真写会被拒绝"
//!    —— 这一条命令是唯一能给出"重建后的完整列表"的地方，把预览拿走等于让用户盲删；
//!    拒绝的载荷里也是**完整计划**（`scopes` / `budget` / `applied` 一个不少）；
//! 6. 写（或者只报计划）。**机器级的改动只算不写**（决策 136）。
//!
//! # 这一层**不做**的事
//!
//! * **不碰机器级 `PATH`**：那要提权，而 tuoen 不做"顺手提权"。
//! * **不调用 `setx`**：1024 字符处静默裁剪 + 永久展开 `%VAR%`。
//! * **不自动清理**重复 / 失效 / 硬编码用户名的条目（决策 24）：它们归 `fix`，
//!   由用户选。
//! * **不给出厂二进制留后门开关**：没有"用假注册表"的环境变量。测试隔离的是
//!   **存储位置**（`LOCALAPPDATA`），不是注册表（`AGENTS.md` 规矩四）。

use std::path::PathBuf;

use tuoen_core::capture::{
    CaptureBundle, CaptureError, CaptureOptions, PathFile, Section, capture,
};
use tuoen_core::detect::DetectContext;
use tuoen_core::pathdiff::{
    PathDiff, PathDiffError, PathDiffOptions, Rebuild, Selection, current_username_from_env,
    current_username_from_process, diff, load_snapshot, rebuild,
};
use tuoen_platform::{BudgetLevel, EnvScope, PathPlan, ProcessEnv, RealProcessEnv, plan_rewrite};

use crate::detect_ctx::Backends;
use crate::doctor_view::UnwiredRootsView;
use crate::envelope::Envelope;
use crate::exit;
use crate::path_cmd::PathCliError;
use crate::path_diff::{PathApplyArgs, PathDiffArgs};
use crate::path_diff_view::{
    BudgetRowView, NothingSelectedView, PathApplyView, PathDiffView, SelectionView,
    print_apply_human, print_diff_human, too_long_notice,
};

/// 装配失败的错误码（与 `doctor` 逐字相同：同一个失败只该有一个名字）。
const UNWIRED_ROOTS: &str = "unwired-roots";

/// 什么都没选的错误码（决策 135）。
const NOTHING_SELECTED: &str = "nothing-selected";

/// 重建后超过 8191 悬崖的错误码（决策 140）。
const TOO_LONG: &str = "too-long";

/// 捕获器没有产出 `path` 段。**这只可能是 bug**，但它不许 panic。
const CAPTURE_MISSING_PATH: &str = "capture-missing-path";

// ─────────────────────────────────────────────────────────────────────────────
// 入口
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen path diff`。返回退出码。
///
/// **它是一条报告，所以成功时永远退出 0** —— 差异多、差异"很严重"都不是失败。
/// 退出 1 只留给"跑不起来"（装配失败 / 采集失败 / 快照读不到）。
pub fn run_diff(args: &PathDiffArgs) -> i32 {
    let backends = Backends::assemble();
    if !backends.roots_are_absolute() {
        return report_unwired(args.json, &backends, "path.diff");
    }

    let ctx = backends.context(false);
    let local = match capture_local_path(&backends, &ctx) {
        Ok(local) => local,
        Err(error) => return report_failure(args.json, "path.diff", &error),
    };
    let target = match load_snapshot(&args.snapshot) {
        Ok(target) => target,
        Err(error) => return report_snapshot_failure(args.json, "path.diff", &error),
    };

    let username = username(&backends);
    let result = diff(
        &local,
        &target,
        ctx.fs,
        &PathDiffOptions {
            current_username: username.value.as_deref(),
        },
    );

    let snapshot = snapshot_text(&args.snapshot);
    let view = PathDiffView::new(&snapshot, username.value.as_deref(), &result);
    if args.json {
        crate::print_json(&Envelope::ok("path.diff", &view));
    } else {
        print_diff_human(&view, username.source);
    }
    exit::SUCCESS
}

/// 跑 `tuoen path apply`。返回退出码。
///
/// **不给 `--only` 也不给 `--pick` = 拒绝执行**（`nothing-selected`，退出码 1）。
pub fn run_apply(args: &PathApplyArgs) -> i32 {
    let selection = Selection::new(args.only.clone(), args.pick.clone());

    let backends = Backends::assemble();
    if !backends.roots_are_absolute() {
        return report_unwired(args.json, &backends, "path.apply");
    }
    // **空选择在读注册表之前就拒绝**：这一条让"没有选择"这件事可以被测到，
    // 而不需要碰这台机器（决策 146 的 CLI 层纪律）。
    if selection.is_empty() {
        return report_nothing_selected(args, &selection);
    }

    let ctx = backends.context(false);
    let local = match capture_local_path(&backends, &ctx) {
        Ok(local) => local,
        Err(error) => return report_failure(args.json, "path.apply", &error),
    };
    let target = match load_snapshot(&args.snapshot) {
        Ok(target) => target,
        Err(error) => return report_snapshot_failure(args.json, "path.apply", &error),
    };

    let username = username(&backends);
    let result = diff(
        &local,
        &target,
        ctx.fs,
        &PathDiffOptions {
            current_username: username.value.as_deref(),
        },
    );
    // `--dry-run` 与真写**共用这一次** diff + rebuild（决策 139）。
    let rebuilt = rebuild(&local, &target, &result, &selection);

    // 一个**没匹配上任何一行**的 `--pick` 必须被说出来：它会让 `applied` 为空而退出码
    // 是 0 —— 那正是"看起来在工作"。契约（`--json` 的键、退出码）由票据钉死，
    // 所以这一条只进 **stderr**：stdout 上的 JSON 一个字节都不变。
    let unmatched = unmatched_picks(&result, &selection);
    if !unmatched.is_empty() {
        eprintln!(
            "tuoen: `--pick` 里这些 id 一条都没匹配上：{}",
            unmatched.join(", ")
        );
        eprintln!(
            "（id 的**唯一**来源是 `tuoen path diff --json` 的 `rows[].id`；\
             没匹配上的 id 不会选中任何东西，所以这一次可能什么都没做。）"
        );
    }

    // ── 长度预算（决策 140 + Lead 的补充裁决）────────────────────────────
    //
    // 档位由 `tuoen_platform::PathBudget::of` 决定（阈值只有那一处定义），这里
    // 只比较那个 slug —— 不抄 8191 / 90% / 75% 这三个数。
    //
    // **真写**在 `exceeded` 时拒绝（`too-long`，退出 1）。**`--dry-run` 不许拒绝**：
    // `path apply` 是唯一能产出"重建后的完整列表"的命令（`path diff` 只给逐条分类），
    // 把一个 `PATH` 已经 9000 字符的用户的预览拿走，他就连"要删哪几条才能降下来"
    // 都无从知道 —— 那不是"预览撒谎"，而是"预览被拿走了"。所以预览照常成功，
    // 只是必须把"真写会被拒绝"说出来（否则通过会被读成"能写"）。
    let exceeded = long_path(&rebuilt.budget.level, args.dry_run);
    if exceeded == LongPath::Refuse {
        return report_too_long(&backends, args, &selection, &rebuilt, &username);
    }
    if exceeded == LongPath::Warn && args.json {
        // 契约在 stdout 上（`--json` 的键由 core 拥有），所以这一句只进 **stderr**；
        // 人类输出那一份在 `print_apply_human` 里，位置紧跟着预算那一段。
        eprintln!(
            "tuoen: {}",
            too_long_notice(&BudgetRowView::from(&rebuilt.budget))
        );
    }

    // 用户级的新列表。**只写用户级**：机器级的改动在上面那次 rebuild 里照常算出来了
    // （`afterRaw` 完整给出），只是不落盘（决策 136）。
    let user_after = user_after(&rebuilt);

    let plan = match plan_rewrite(backends.env(), EnvScope::User, &user_after) {
        Ok(plan) => plan,
        Err(error) => {
            return report_failure(
                args.json,
                "path.apply",
                &PathCliError {
                    code: crate::path_cmd::platform_error_code(&error),
                    message: format!("算不出写回计划：{error}"),
                },
            );
        }
    };

    let snapshot = snapshot_text(&args.snapshot);
    if args.dry_run {
        // **同一个计划对象**，只是不调 `apply_rewrite`（决策 139）。
        let view = PathApplyView::new(
            &snapshot,
            username.value.as_deref(),
            true,
            &selection,
            &rebuilt,
            &plan,
            None,
        );
        return finish(args.json, &view, username.source, plan.will_write());
    }

    let applied = match tuoen_platform::apply_rewrite(backends.registry(), &plan, broadcast) {
        Ok(applied) => applied,
        Err(error) => {
            return report_failure(
                args.json,
                "path.apply",
                &PathCliError {
                    code: crate::path_cmd::platform_error_code(&error),
                    message: format!("写用户级 PATH 失败：{error}"),
                },
            );
        }
    };
    let view = PathApplyView::new(
        &snapshot,
        username.value.as_deref(),
        false,
        &selection,
        &rebuilt,
        &plan,
        Some(&applied),
    );
    finish(args.json, &view, username.source, plan.will_write())
}

/// 计划做好了：输出 + 退出码。两条路径共用，所以它们的输出契约不可能漂移。
///
/// `plan_will_write` 是 `PathPlan::will_write()` —— **只进人类输出**，不进
/// [`PathApplyView`]（`--json` 的键由 core 拥有，多一个键就是第二份事实）。
fn finish(json: bool, view: &PathApplyView, username_source: &str, plan_will_write: bool) -> i32 {
    if json {
        crate::print_json(&Envelope::ok("path.apply", view));
    } else {
        print_apply_human(view, username_source, plan_will_write);
    }
    exit::SUCCESS
}

/// `--pick` 里那些**一条都没匹配上**的 id（去重、按用户给的顺序）。
///
/// 判据是"与任何一行的 `id` 逐字相同"，与 [`Selection::selects`] 用的是同一份 `id`。
fn unmatched_picks(result: &PathDiff, selection: &Selection) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pick in selection.picks() {
        if result.rows.iter().any(|row| row.id == *pick) || out.iter().any(|seen| seen == pick) {
            continue;
        }
        out.push(pick.clone());
    }
    out
}

/// 广播闭包。**只在真的写了之后被调用** —— `apply` 保证这一点
/// （它不写的时候直接返回 `wrote: false`，一次广播都不发）。
///
/// `pub(crate)` 是为了让 `restore` 的 `env` 段用**同一个**广播函数：广播是全局副作用
/// （打到每一个顶层窗口），它只能有一处定义。
pub(crate) fn broadcast() -> usize {
    tuoen_platform::sys::broadcast_environment_change()
}

// ─────────────────────────────────────────────────────────────────────────────
// 本机侧 / 目标侧 / 当前用户名
// ─────────────────────────────────────────────────────────────────────────────

/// 本机侧：**在内存里**跑一次 `capture`（决策 126），一个字节都不落盘。
///
/// `out_dir` 传空路径：它只被 `write_bundle` 用到，而这一族一次都不落盘。
/// 时间戳照样要给（`CaptureOptions` 要求它），但它只活在内存里。
///
/// 我们自己的两个根（存储根 + shim 目录）必须给：它们是 `owner = "tuoen"` 的**唯一**
/// 判据，而 `owner` 会逐条进 `--json`。
///
/// `sections` 是**调用方**给的：`path diff` / `path apply` 只要 `Section::Path`，
/// 而 `restore` 要四段（它比的是整份 `tuoen.d/`）。抽成一个函数是为了让"怎么装六个
/// 后端 + 怎么给两个根"只有一处 —— 第二份装配会让两份事实慢慢漂移。
pub(crate) fn capture_local(
    backends: &Backends,
    ctx: &DetectContext<'_>,
    sections: Vec<Section>,
) -> Result<CaptureBundle, CaptureError> {
    let opts = CaptureOptions::only(PathBuf::new(), &tuoen_store::now_rfc3339(), sections)
        .with_tuoen_root(backends.store_root())
        .with_tuoen_root(backends.shim_dir());

    // 错误**原样**返回：文案由调用方按自己的口径写（`path` 那一族说的是 "PATH"，
    // `restore` 说的是"整机状态"）—— 把文案烤进共用件就是让两个命令说同一句话。
    capture(ctx, &opts)
}

/// 本机侧的 `PATH` 那一段（`path diff` / `path apply` 要的形状）。
fn capture_local_path(
    backends: &Backends,
    ctx: &DetectContext<'_>,
) -> Result<PathFile, PathCliError> {
    let bundle =
        capture_local(backends, ctx, vec![Section::Path]).map_err(|error| PathCliError {
            code: error.code(),
            message: format!("采集本机 PATH 失败：{error}"),
        })?;
    bundle.path.ok_or_else(|| PathCliError {
        code: CAPTURE_MISSING_PATH,
        message: "采集本机 PATH 失败：捕获器没有产出 `path` 段（这是一个 bug，请报告）。"
            .to_owned(),
    })
}

/// 当前用户名与**它的来源**。
///
/// 来源只用于人类输出（`--json` 里只有值），但它必须说出来：同一份快照在不同的
/// 当前用户名下**类会不同**（用户名依赖的行只有在真的有旧名可换时才归 `fix`）——
/// 不说来源，用户会以为这个工具不稳定。
///
/// 顺序是决策 145：**进程环境优先**（`USERPROFILE` 的最后一段，退 `USERNAME`），
/// 用户级注册表块是退路。真机实测 `HKCU\Environment` 里**既没有** `USERPROFILE`
/// 也没有 `USERNAME` —— 只读注册表块的实现会得到 `None`，于是**一条都不重写**，
/// 而报告看起来完全正常。
pub(crate) struct Username {
    pub(crate) value: Option<String>,
    pub(crate) source: &'static str,
}

/// 当前用户名 + **它的来源** —— `restore` 的 path 段复用同一个函数（决策 145 的判据
/// 只能有一处）。
pub(crate) fn username(backends: &Backends) -> Username {
    if let Some(value) = current_username_from_process(backends.process_env()) {
        return Username {
            value: Some(value),
            source: process_username_source(backends.process_env()),
        };
    }
    if let Some(value) = current_username_from_env(backends.env()) {
        return Username {
            value: Some(value),
            source: "用户级注册表块",
        };
    }
    Username {
        value: None,
        source: process_username_source(backends.process_env()),
    }
}

/// 进程环境里那个名字具体来自哪个变量。**只为了把话说准**。
///
/// 判据与 core 的一致：`USERPROFILE` 的**最后一段**非空才算可用（`C:\` 那种没有名字）。
fn process_username_source(process_env: &RealProcessEnv) -> &'static str {
    let vars = process_env.vars();
    let get = |name: &str| {
        vars.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    };
    match (get("USERPROFILE"), get("USERNAME")) {
        (Some(profile), _) if has_last_segment(profile) => "进程环境 USERPROFILE",
        (_, Some(_)) => "进程环境 USERNAME",
        _ => "进程环境（两个变量都没有）",
    }
}

/// `USERPROFILE` 的最后一段是不是一个可用的名字（与 core 的判据一致：空段不算）。
fn has_last_segment(value: &str) -> bool {
    value
        .rsplit(['\\', '/'])
        .next()
        .is_some_and(|name| !name.trim().is_empty())
}

/// 用户给的那个路径，**原样**（相对路径就还是相对路径）。
pub(crate) fn snapshot_text(path: &std::path::Path) -> String {
    path.display().to_string()
}

// ─────────────────────────────────────────────────────────────────────────────
// 失败路径
// ─────────────────────────────────────────────────────────────────────────────

/// 采集 / 计划 / 写回失败。**错误码稳定、消息中文**，两者分开的理由与 `envelope` 一致。
fn report_failure(json: bool, command: &'static str, error: &PathCliError) -> i32 {
    if json {
        crate::print_json(&Envelope::err(command, error.code, error.message.clone()));
    } else {
        eprintln!("tuoen: {}", error.message);
    }
    exit::RUNTIME_ERROR
}

/// 读目标快照失败。**两个错误码必须分得开**（core 的 `PathDiffError`）：
/// `path-snapshot-io` 是"文件不在/没权限"，`path-snapshot-toml` 是"文件在那儿但内容变了"
/// —— 用户要做的下一步完全不同。
fn report_snapshot_failure(json: bool, command: &'static str, error: &PathDiffError) -> i32 {
    if json {
        crate::print_json(&Envelope::err(command, error.code(), error.to_string()));
    } else {
        eprintln!("tuoen: {error}");
    }
    exit::RUNTIME_ERROR
}

/// 装配失败：算不出我们自己的根位置（两个根里有不是绝对路径的）。
///
/// # 为什么这是"跑不起来"，而不是"照跑不误再少报几条"
///
/// `Store::at_default_location()` 在 `%LOCALAPPDATA%` 与 `%USERPROFILE%` 都读不到时会
/// 退到相对路径 `.tuoen\store`（store 层的既定行为：它**不 panic**）。而对这一票来说，
/// 相对根会**静默**污染两类判断：
///
/// * `owner`（哪些 `PATH` 条目是我们自己的）比较的是 `PATH` 上的绝对条目与我们的相对根
///   —— 恒不相等，于是我们自己的条目被报成"别人的"；
/// * 遮蔽检测比较 shim 目录与 `PATH` 顺序上的目录 —— 同样是恒不相等，于是
///   "我们的 shim 被抢了名字"这一类发现**一条都不报**。
///
/// 两者都会让报告看起来完全正常。所以宁可失败 —— 而且失败里要带出**已经确定的事实**
/// （算出来的那两个根），让读的人一眼看出是环境缺了变量，而不是工具坏了。
///
/// # 为什么是 `partial` 而不是 `err`
///
/// 与 `capture_cmd::report_failure` / `doctor_cmd::report_unwired` 同一条理由：
/// 把"算出来的根长什么样"丢掉之后，消费者只剩一句"失败了"。而 [`UnwiredRootsView`]
/// 里没有 `counts` / `rows` / `scopes` —— diff 与重建**根本没跑**，
/// 报几个 0 就是在说"我看过了，什么都没发现"。
fn report_unwired(json: bool, backends: &Backends, command: &'static str) -> i32 {
    let message = format!(
        "算不出 tuoen 自己的根位置：`%LOCALAPPDATA%` 与 `%USERPROFILE%` 都读不到，\
         于是存储根退成了相对路径 `{}`。\n\
         相对的根本分不出「这条 PATH 条目是不是我们自己的」与「我们的 shim 有没有被\
         别的目录抢在前面」—— 那两个判断会静默地变成「不是」与「没有」，\
         而报告看起来完全正常。所以这里宁可失败：请先设好 `%LOCALAPPDATA%` 再跑一次。",
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

/// 什么都没选：**拒绝执行**（决策 135）。
///
/// # 为什么失败载荷里没有 `scopes` / `rows`
///
/// 那是"执行的结果"，而这一次**没有执行**。报一个空数组是在说"我看过了，
/// 什么都没做"—— 两件事完全不同（"没执行"与"执行了但没改动"）。
/// 所以 [`NothingSelectedView`] 里只有"还没执行之前就已经确定的事实"。
///
/// # 为什么这一条排在读注册表之前
///
/// 拒绝的理由（两个空列表）在**任何机器上都成立**，所以没有理由先去读一遍注册表。
/// 这同时让契约测试能覆盖它而不碰这台机器（决策 146 的 CLI 层纪律）。
fn report_nothing_selected(args: &PathApplyArgs, selection: &Selection) -> i32 {
    let message = "没有选任何东西：`--only <类>` 与 `--pick <id>` 都空。\n\
         `path apply` 动的是你**整条 `PATH`**，所以它要求你把意图说出来 —— \
         默认值一旦存在，「顺手删掉几条」就会成为默认行为（决策 135）。\n\
         先看一眼：`tuoen path diff <snapshot>`；再按类或按 id 选：\
         `--only add` / `--pick user:+16`。"
        .to_owned();
    let data = NothingSelectedView {
        snapshot: snapshot_text(&args.snapshot),
        dry_run: args.dry_run,
        selection: SelectionView::from(selection),
    };
    if args.json {
        crate::print_json(&Envelope::partial(
            "path.apply",
            NOTHING_SELECTED,
            message,
            &data,
        ));
    } else {
        eprintln!("tuoen: {message}");
    }
    exit::RUNTIME_ERROR
}

/// 长度预算的档位 + 这一次的模式 → 该怎么做（Lead 的补充裁决）。
///
/// 抽成纯函数只为一件事：把"**`--dry-run` 不许被 `exceeded` 拒绝**"这条裁决钉在一处，
/// 并且有一个**不碰机器**的用例（集成测试那一条要用一份 9000 字符的快照）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LongPath {
    /// 档位没到 `exceeded`：照常写。
    Write,
    /// 到了 `exceeded` 但是 `--dry-run`：**计划照给**，外加一句"真写会被拒绝"。
    Warn,
    /// 到了 `exceeded` 且是真写：拒绝（`too-long`，退出 1），载荷里仍然是完整计划。
    Refuse,
}

/// 档位 slug 由 `tuoen_platform::PathBudget::of` 决定 —— 这里只比较它，不抄阈值。
fn long_path(level: &str, dry_run: bool) -> LongPath {
    if level != BudgetLevel::Exceeded.as_str() {
        LongPath::Write
    } else if dry_run {
        LongPath::Warn
    } else {
        LongPath::Refuse
    }
}

/// 重建之后超过 8191 悬崖：**拒绝写入**（决策 140）。
///
/// 票据只要求"检查并告警"。但 `exceeded` 时写进去的结果是 `cmd.exe` **静默截断**
/// （整条 `PATH` 一次性全部失效，用户不会知道少了哪几条）—— 那正是这一票要消灭的
/// 那类故障。拒绝并把"还差多少、要少几条"讲清楚，比写进去再让用户猜要好。
///
/// **只有真写会走到这里**：`--dry-run` 在 `exceeded` 时照常成功（Lead 的补充裁决），
/// 否则一个 `PATH` 已经 9000 字符的用户**永远看不到自己的重建结果**，连"要删哪几条
/// 才能降下来"都无从知道 —— 那不是"预览撒谎"，而是"预览被拿走了"。
///
/// 载荷是**完整计划**：`scopes` / `budget` / `applied` 一个不少 —— 拒绝的理由就在那份
/// 计划里（哪几条把它顶过了悬崖）。[`PathApplyView`] 另外需要一个 `PathPlan` 来报
/// `regType` / `chars`：算得出就用真的（那是"本来会写成什么样"），算不出（快照里带一条
/// 含 `;` 的条目那种）就退回现状（[`no_write_plan`]）。
fn report_too_long(
    backends: &Backends,
    args: &PathApplyArgs,
    selection: &Selection,
    rebuilt: &Rebuild,
    username: &Username,
) -> i32 {
    let budget = &rebuilt.budget;
    let over = budget.effective_chars.saturating_sub(budget.cliff);
    let entries = rebuilt
        .scopes
        .iter()
        .map(|scope| scope.after_entries.len())
        .sum::<usize>();
    let drop = entries_to_drop(rebuilt, over);
    let message = format!(
        "重建之后的注册表原文是 {} 字符（机器级 {} + 用户级 {}），\
         超过 `cmd.exe` 的悬崖 {} 字符 —— 超了 {over} 字符。\n\
         超过悬崖之后 `cmd.exe` 会**完全忽略整条 `PATH`** —— 不是少几条，是所有命令一起失效。\n\
         所以这一次**拒绝写入**：请先少选几条（换一个 `--only`，或用 `--pick` 一条条点）。\
         按最长的先丢是下界，**至少要少 {drop} 条条目**（重建后一共 {entries} 条）。\n\
         （这一次的选择：类 [{}] · 条目 [{}]。）",
        budget.effective_chars,
        budget.raw_machine_chars,
        budget.raw_user_chars,
        budget.cliff,
        selection
            .classes()
            .iter()
            .map(|class| class.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        selection.picks().join(", ")
    );

    // 计划**完整给出**：拒绝的理由就在那份计划里（哪几条把它顶过了悬崖）。
    let plan = plan_rewrite(backends.env(), EnvScope::User, &user_after(rebuilt))
        .unwrap_or_else(|_| no_write_plan(rebuilt));
    let plan_view = PathApplyView::new(
        &snapshot_text(&args.snapshot),
        username.value.as_deref(),
        args.dry_run,
        selection,
        rebuilt,
        &plan,
        None,
    );
    if args.json {
        crate::print_json(&Envelope::partial(
            "path.apply",
            TOO_LONG,
            message,
            &plan_view,
        ));
    } else {
        print_apply_human(&plan_view, username.source, plan.will_write());
        eprintln!("tuoen: {message}");
    }
    exit::RUNTIME_ERROR
}

/// 用户级重建后的条目列表 —— **只写用户级**（决策 136）。
///
/// 机器级的改动在 `rebuild` 里照常算出来（`afterRaw` 完整给出），只是不落盘。
pub(crate) fn user_after(rebuilt: &Rebuild) -> Vec<String> {
    rebuilt
        .scopes
        .iter()
        .find(|scope| scope.scope == EnvScope::User)
        .map_or_else(Vec::new, |scope| scope.after_entries.clone())
}

/// 至少要少几条条目才能回到悬崖以下 —— **下界**（贪心：最长的先丢）。
///
/// 对用户来说，"还差 300 字符"不是能照着做的动作（300 字符是几条？），"至少要少 3 条"
/// 才是。这个数是下界，有两重理由：丢掉一条还会连带省下它的分隔符；而且真删起来用户
/// 未必愿意丢最长的那几条。
fn entries_to_drop(rebuilt: &Rebuild, over: usize) -> usize {
    if over == 0 {
        return 0;
    }
    let mut lengths: Vec<usize> = rebuilt
        .scopes
        .iter()
        .flat_map(|scope| scope.after_entries.iter())
        .map(|entry| entry.chars().count())
        .collect();
    lengths.sort_unstable_by(|a, b| b.cmp(a));
    let mut freed = 0usize;
    for (index, length) in lengths.iter().enumerate() {
        freed += length;
        if freed >= over {
            return index + 1;
        }
    }
    lengths.len()
}

/// 一份**不写任何东西**的占位计划：**只在算不出真计划时**用它。
///
/// `PathApplyView` 需要 `PathPlan` 只是为了 `--dry-run` 时那两个数（`regType` /
/// `chars`）。正常路径上 `too-long` 的载荷用的是**真的**写回计划（"本来会写成什么样"
/// 是用户判断"少选哪几条"的依据）；只有 `plan_rewrite` 自己失败时（快照里带一条含 `;`
/// 的条目那种）才退回这一份：`will_write() == false`，`regType` / `chars` 报**现状**。
fn no_write_plan(rebuilt: &Rebuild) -> PathPlan {
    let user = rebuilt
        .scopes
        .iter()
        .find(|scope| scope.scope == EnvScope::User);
    let before_raw = user.map_or_else(String::new, |scope| scope.before_raw.clone());
    let before_type = user
        .and_then(|scope| scope.before_type)
        .unwrap_or(tuoen_platform::RegType::Sz);
    PathPlan {
        scope: EnvScope::User,
        before_raw: before_raw.clone(),
        after_raw: before_raw,
        before_type,
        after_type: before_type,
        changes: Vec::new(),
        changes_value: false,
        budget_after: tuoen_platform::PathBudget::of(
            rebuilt.budget.effective_chars,
            rebuilt.budget.effective_chars,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_error_codes_are_stable_lowercase_kebab_ascii() {
        for code in [
            UNWIRED_ROOTS,
            NOTHING_SELECTED,
            TOO_LONG,
            CAPTURE_MISSING_PATH,
        ] {
            assert!(
                !code.is_empty()
                    && code
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{code}` 不是小写 kebab ASCII"
            );
        }
        // 与 `doctor` 逐字相同：同一个失败只该有一个名字。
        assert_eq!(UNWIRED_ROOTS, "unwired-roots");
    }

    #[test]
    fn the_username_source_says_which_variable_was_used() {
        // 只读注册表块的实现会拿到 `None`（真机实测），所以这一句是"为什么一条都没重写"
        // 的唯一解释。
        assert!(!process_username_source(&RealProcessEnv::new()).is_empty());
        assert!(has_last_segment(r"C:\Users\a"));
        assert!(!has_last_segment(r"C:\"));
        assert!(!has_last_segment(""));
    }

    #[test]
    fn unmatched_picks_names_only_the_ids_that_matched_nothing() {
        use tuoen_core::capture::Existence;
        use tuoen_core::pathdiff::DiffClass;
        use tuoen_core::pathdiff::test_support::{diff_of, fake_fs, path_file, selection};

        let local = path_file(&[(EnvScope::User, r"C:\a", Existence::Yes)]);
        let target = path_file(&[
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\b", Existence::Yes),
        ]);
        let fs = fake_fs(&[]);
        let result = diff_of(&local, &target, &fs);
        let add_id = result
            .rows
            .iter()
            .find(|row| row.class == DiffClass::Add)
            .expect("目标里多出来的那条是 `add`")
            .id
            .clone();
        assert!(add_id.starts_with("user:+"));

        // 重复的 id 只报一次，顺序按用户给的。
        let picked = selection(&[], &[&add_id, "user:9999", "user:9999"]);
        assert_eq!(
            unmatched_picks(&result, &picked),
            vec!["user:9999".to_owned()]
        );
        // 按类选的时候没有任何 pick 可失配。
        assert!(unmatched_picks(&result, &selection(&[DiffClass::Add], &[])).is_empty());
    }

    #[test]
    fn a_dry_run_is_never_refused_for_being_too_long() {
        // Lead 的补充裁决：`path apply` 是唯一能给出"重建后的完整列表"的命令，
        // 所以 `exceeded` 的**预览**必须照常成功（外加一句"真写会被拒绝"）。
        assert_eq!(long_path("exceeded", true), LongPath::Warn);
        assert_eq!(long_path("exceeded", false), LongPath::Refuse);
        // 其余三档照常写 —— 阈值（8191 / 90% / 75%）只在 platform 那一处定义。
        for level in ["ok", "warning", "critical"] {
            assert_eq!(long_path(level, true), LongPath::Write, "{level}");
            assert_eq!(long_path(level, false), LongPath::Write, "{level}");
        }
    }

    #[test]
    fn the_entries_to_drop_count_is_a_lower_bound() {
        // 三条的长度：`C:\a` = 4、`C:\bbbbbbbbbb` = 13、`C:\cc` = 5。
        // 贪心按**最长先丢**，所以边界值要照着 13 / 13+5=18 / 22 来写。
        let rebuilt = rebuild_with(&[r"C:\a", r"C:\bbbbbbbbbb", r"C:\cc"]);
        assert_eq!(entries_to_drop(&rebuilt, 1), 1, "差 1 字符：丢最长的那条");
        assert_eq!(entries_to_drop(&rebuilt, 13), 1, "正好等于最长那条");
        assert_eq!(entries_to_drop(&rebuilt, 14), 2, "13 + 5 = 18 ≥ 14");
        assert_eq!(entries_to_drop(&rebuilt, 18), 2, "正好等于前两条之和");
        assert_eq!(entries_to_drop(&rebuilt, 19), 3, "得三条全丢");
        // 全都丢掉也不够：报"全部"（这个数仍然是**条数**，不是字符数）。
        assert_eq!(entries_to_drop(&rebuilt, 9_999), 3);
        // 没超就一条都不用丢。
        assert_eq!(entries_to_drop(&rebuilt, 0), 0);
    }

    /// 造一个只有用户级作用域的 [`Rebuild`]（`entries_to_drop` 只关心 `after_entries`）。
    fn rebuild_with(after_entries: &[&str]) -> Rebuild {
        let entries: Vec<String> = after_entries
            .iter()
            .map(|entry| (*entry).to_owned())
            .collect();
        Rebuild {
            scopes: vec![tuoen_core::pathdiff::RebuiltScope {
                scope: EnvScope::User,
                before_raw: entries.join(";"),
                after_raw: entries.join(";"),
                before_entries: entries.clone(),
                after_entries: entries,
                before_type: Some(tuoen_platform::RegType::Sz),
                applied: Vec::new(),
                untouched: 0,
                requires_elevation: false,
            }],
            budget_before: budget_row("ok", 0),
            budget: budget_row("ok", 0),
            no_op_classes: Vec::new(),
        }
    }

    fn budget_row(level: &str, effective_chars: usize) -> tuoen_core::capture::PathBudgetRow {
        tuoen_core::capture::PathBudgetRow {
            raw_user_chars: 0,
            raw_machine_chars: 0,
            effective_chars,
            cliff: 8191,
            remaining: 0,
            level: level.to_owned(),
        }
    }

    #[test]
    fn a_too_long_plan_never_asks_for_a_write() {
        // 这一份是**退路**：`plan_rewrite` 自己失败时才用（快照里带一条含 `;` 的条目
        // 那种）。它必须自己声明"什么都不写"，否则载荷里就摆着一份超过悬崖的 after。
        let mut rebuilt = rebuild_with(&[r"C:\a", r"C:\b", r"C:\c"]);
        rebuilt.budget = budget_row("exceeded", 8192);
        let plan = no_write_plan(&rebuilt);
        assert!(!plan.will_write(), "占位计划不许写任何东西");
        assert_eq!(plan.after_raw, plan.before_raw);
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn the_user_after_list_is_the_user_scope_only() {
        let rebuilt = rebuild_with(&[r"C:\a", r"C:\b"]);
        assert_eq!(
            user_after(&rebuilt),
            vec![r"C:\a".to_owned(), r"C:\b".to_owned()]
        );
        // 没有用户级作用域（`has_content()` 全 false 时 rebuild 不给这一节）：
        // 报空列表 —— `plan_rewrite` 拿它算出来的是"清空用户级 `PATH`"，
        // 而那正是重建的结果，不是这里该替用户决定的事。
        let empty = Rebuild {
            scopes: Vec::new(),
            ..rebuild_with(&[])
        };
        assert!(user_after(&empty).is_empty());
    }
}
