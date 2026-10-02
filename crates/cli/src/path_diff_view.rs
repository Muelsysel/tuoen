//! `tuoen path diff` / `tuoen path apply` 的 `--json` 形状与人类输出。
//!
//! # 键名归 core 所有，这里只负责"把它们放进 `data`"
//!
//! `rows` / `counts` / `rewrites` / `typos` / `scopes` / `applied` / `noOpClasses`
//! 都是**直接序列化 core 的类型**（`crates/core/src/pathdiff.rs`）。理由写在那个模块的
//! 文档里：`path diff`（只读）、`path apply --dry-run`（只读）与 `path apply`（会写）
//! 三条路径产出的是**同一个对象**，所以键只能有一处定义。在这里重抄一遍
//! `beforeEntries` 这种字面量，等于给"预览里的键"与"执行里的键"各开一个来源，
//! 而它们漂移的表现是脚本读到 `null` 而不是报错。
//!
//! # 唯一需要包一层的是长度预算
//!
//! `PathBudgetRow` 是 `path.toml` 的**磁盘模型**（字段名是 snake_case，因为它同时要当
//! TOML 的键），而 `--json` 的键是 camelCase。所以这里有一个显式的
//! [`BudgetRowView`] —— 它不是"第二份事实"，只是同一批数字的另一种键名，
//! 与 `capture_cmd::PathCounts` / `path_view::BudgetView` 的做法一致。
//!
//! # 时间戳一个都不许进 JSON
//!
//! 本机侧那份 `PathFile` 带着 `captured_at`，而它**只活在内存里**（决策 126）：
//! 进 `--json` 的每一个数字都从 `PathDiff` / `Rebuild` 里来，两者都没有时间戳。
//! 于是同一条命令在同一台机器上跑两次**逐字节相同** —— 验收脚本与契约测试都钉这一条。

use serde::Serialize;
use tuoen_core::capture::PathBudgetRow;
use tuoen_core::pathdiff::{
    AppliedRow, DiffClass, DiffCounts, PathDiff, PathDiffRow, Rebuild, RebuiltScope, Rewrite,
    Selection, TypoSuspect,
};
use tuoen_platform::{EnvScope, PathApplied, PathPlan};

use crate::path_view::{reg_type_slug, scope_label};

/// `--json` 里 `visibility` 的**唯一**取值。
///
/// 决策 138 的机器可读形态：环境块在 `CreateProcess` 时复制，所以写完之后
/// **只有新起的进程**能拿到 —— 人类输出里是那两段中文，脚本读的是这个 slug。
pub const VISIBILITY_RESTART_REQUIRED: &str = "restart-required";

// ─────────────────────────────────────────────────────────────────────────────
// `path diff --json`
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen path diff --json` 的载荷。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathDiffView {
    /// 用户给的那个快照路径，**原样**（相对路径就还是相对路径）。
    pub snapshot: String,
    /// 当前用户名。**`null` 就是真的取不到**（那就一条都不重写）。
    ///
    /// 它不进 `Option` 的"缺省即不出键"规则：`null` 与"没有这个键"在这里是两件事，
    /// 而"取不到用户名"是一个必须被看见的结论 —— 只读注册表块的实现会得到它。
    pub current_username: Option<String>,
    /// 各类的条数。`sum(counts) == rows.len()`。
    pub counts: DiffCounts,
    /// 逐条。**core 拥有键名**（`local` / `target` / `also` / `rewriteTo` / `suggestion`）。
    pub rows: Vec<PathDiffRow>,
    /// 用户名重写清单，逐条报告（决策 134）。
    pub rewrites: Vec<Rewrite>,
    /// 拼写疑似清单，**只报告**（决策 133）。
    pub typos: Vec<TypoSuspect>,
}

impl PathDiffView {
    /// 从一次 diff 组装。`snapshot` 是**用户的输入**（不在 `PathDiff` 里）。
    #[must_use]
    pub fn new(snapshot: &str, current_username: Option<&str>, diff: &PathDiff) -> Self {
        Self {
            snapshot: snapshot.to_owned(),
            current_username: current_username.map(ToOwned::to_owned),
            counts: diff.counts,
            rows: diff.rows.clone(),
            rewrites: diff.rewrites.clone(),
            typos: diff.typos.clone(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `path apply --json`
// ─────────────────────────────────────────────────────────────────────────────

/// 长度预算的 `--json` 形状。
///
/// **不能直接序列化 [`PathBudgetRow`]**：那是 `path.toml` 的磁盘模型，键名是
/// snake_case（`raw_user_chars`），而 `--json` 的契约是 camelCase（`rawUserChars`）。
/// 这不是第二份事实，只是同一批数字的另一种键名。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BudgetRowView {
    /// 用户级注册表原文的字符数（未展开）。
    pub raw_user_chars: usize,
    /// 机器级注册表原文的字符数（未展开）。
    pub raw_machine_chars: usize,
    /// 注册表口径的合计长度（`rawUserChars + rawMachineChars`）。**下界**。
    pub effective_chars: usize,
    /// `cmd.exe` 的悬崖。
    pub cliff: usize,
    /// 距悬崖还剩多少字符。
    pub remaining: usize,
    /// 稳定 slug：`ok` / `warning` / `critical` / `exceeded`。
    pub level: String,
}

impl From<&PathBudgetRow> for BudgetRowView {
    fn from(row: &PathBudgetRow) -> Self {
        Self {
            raw_user_chars: row.raw_user_chars,
            raw_machine_chars: row.raw_machine_chars,
            effective_chars: row.effective_chars,
            cliff: row.cliff,
            remaining: row.remaining,
            level: row.level.clone(),
        }
    }
}

/// 用户选了什么（`--only` 的 slug + `--pick` 的 id 逐字）。
#[derive(Debug, Serialize)]
pub struct SelectionView {
    /// 稳定 slug，**顺序就是用户给的顺序**。
    pub classes: Vec<&'static str>,
    /// `--pick` 的原样列表。
    pub picks: Vec<String>,
}

impl From<&Selection> for SelectionView {
    fn from(selection: &Selection) -> Self {
        Self {
            classes: selection.classes().iter().map(|c| c.as_str()).collect(),
            picks: selection.picks().to_vec(),
        }
    }
}

/// 一条**需要提权**的机器级改动（决策 136）。
///
/// 机器级的选中改动**照常算进重建结果**（`afterRaw` 完整给出），只是不落盘 ——
/// 静默提权是这一票最不该做的事。用户能拿这份清单去别处用，或等 `restore`。
#[derive(Debug, Serialize)]
pub struct ElevationRow {
    /// 哪一行（`PathDiffRow::id`）。
    pub id: String,
    /// 因为哪一类被动。
    pub class: DiffClass,
    /// 结果值。
    pub value: String,
}

impl From<&AppliedRow> for ElevationRow {
    fn from(row: &AppliedRow) -> Self {
        Self {
            id: row.id.clone(),
            class: row.class,
            value: row.value.clone(),
        }
    }
}

/// `tuoen path apply --json` 的载荷。
///
/// **`--dry-run` 与真写是同一个形状**，区别只在 `dryRun` / `wrote` /
/// `regType` / `chars` / `broadcastReplies`（决策 139）。给演练单独一套形状会让消费者
/// 写两套解析，而"我只想看会做什么"与"我真做了"要读的字段其实是同一批。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathApplyView {
    /// 用户给的那个快照路径，**原样**。
    pub snapshot: String,
    /// 当前用户名（`null` = 真的取不到）。
    pub current_username: Option<String>,
    /// 这一次是演练吗。
    pub dry_run: bool,
    /// 用户选了什么。
    pub selection: SelectionView,
    /// 真的写了吗。`--dry-run` 恒为 `false`（那不是"写失败了"）。
    pub wrote: bool,
    /// 写进去的类型（`sz` / `expand-sz`）。**演练时用计划的 after 类型**。
    pub reg_type: &'static str,
    /// 写进去的字符数。**演练时用 `afterRaw` 的长度**。
    pub chars: usize,
    /// 广播回执。**演练时恒为 0**（一次广播都没发）。
    pub broadcast_replies: usize,
    /// 恒为 `restart-required`（决策 138 的机器可读形态）。
    pub visibility: &'static str,
    /// 被选中、但**没有任何变换可做**的类（决策 144）。`case-only` 永远在这里。
    pub no_op_classes: Vec<DiffClass>,
    /// 机器级的选中改动：**算出来了，但不会写**（要提权）。
    pub requires_elevation: Vec<ElevationRow>,
    /// 重建前的长度预算。
    pub budget_before: BudgetRowView,
    /// 重建后的长度预算。**注册表口径的下界**，不含进程注入项。
    pub budget: BudgetRowView,
    /// 逐作用域的重建结果（**core 拥有键名**）。
    pub scopes: Vec<RebuiltScope>,
}

impl PathApplyView {
    /// 从重建结果 + 平台计划 +（真写时的）落盘结果组装。
    ///
    /// # `applied` 为 `None` 时那几个数字从哪来
    ///
    /// 决策 139：`--dry-run` 不是"另写一条只读分支"，它就是同一份计划少调一次
    /// `apply`。所以演练里的 `regType` 用**计划的** after 类型、`chars` 用
    /// `afterRaw` 的长度、`broadcastReplies` 为 0（一次广播都没发）——
    /// 这样"预览里的数"与"真写下去的字节"读的是同一个来源。
    #[must_use]
    pub fn new(
        snapshot: &str,
        current_username: Option<&str>,
        dry_run: bool,
        selection: &Selection,
        rebuild: &Rebuild,
        plan: &PathPlan,
        applied: Option<&PathApplied>,
    ) -> Self {
        let (wrote, reg_type, chars, broadcast_replies) = match applied {
            Some(applied) => (
                applied.wrote,
                reg_type_slug(&applied.reg_type),
                applied.chars,
                applied.broadcast_replies,
            ),
            None => (
                false,
                reg_type_slug(&plan.after_type),
                plan.after_raw.chars().count(),
                0,
            ),
        };

        let requires_elevation = rebuild
            .scopes
            .iter()
            .filter(|scope| scope.scope == EnvScope::Machine)
            .flat_map(|scope| scope.applied.iter().map(ElevationRow::from))
            .collect();

        Self {
            snapshot: snapshot.to_owned(),
            current_username: current_username.map(ToOwned::to_owned),
            dry_run,
            selection: SelectionView::from(selection),
            wrote,
            reg_type,
            chars,
            broadcast_replies,
            visibility: VISIBILITY_RESTART_REQUIRED,
            no_op_classes: rebuild.no_op_classes.clone(),
            requires_elevation,
            budget_before: BudgetRowView::from(&rebuild.budget_before),
            budget: BudgetRowView::from(&rebuild.budget),
            scopes: rebuild.scopes.clone(),
        }
    }
}

/// **空选择**的失败载荷：只带"还没执行"之前就已经确定的事实。
///
/// 它刻意**没有** `scopes` / `rows`：那是"执行的结果"，而这一次**没有执行** ——
/// 报一个空数组是在说"我看过了，什么都没做"，两件事完全不同（决策 135）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NothingSelectedView {
    /// 用户给的那个快照路径，**原样**。
    pub snapshot: String,
    /// 这一次是不是演练。
    pub dry_run: bool,
    /// 用户选了什么（两个空数组就是拒绝的原因本身）。
    pub selection: SelectionView,
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen path diff` 的人类输出。
///
/// `username_source` 是"当前用户名从哪来"那一句的**后半段**（如 `进程环境 USERPROFILE`）。
/// 它必须说出来：同一份快照在不同的当前用户名下**类会不同**（用户名依赖的行只有在
/// 真的有旧名可换时才归 `fix`），不说来源，用户会以为这个工具不稳定。
pub fn print_diff_human(view: &PathDiffView, username_source: &str) {
    println!("PATH 逐条 diff —— 目标快照：{}", view.snapshot);
    println!(
        "{}",
        username_line(view.current_username.as_deref(), username_source)
    );
    println!(
        "  （同一份快照在不同的当前用户名下**类会不同** —— 用户名依赖的行只有在真的有\
         旧名可换时才归 `fix`，其余按它别的性质归类。）"
    );
    println!();

    println!(
        "逐条 {} 行（本机侧的行 + 只有目标侧的行）：",
        view.rows.len()
    );
    let counts = &view.counts;
    println!(
        "  keep {} · add {} · remove {} · move {} · fix {} · case-only {}",
        counts.keep, counts.add, counts.remove, counts.move_, counts.fix, counts.case_only
    );
    println!();

    for row in &view.rows {
        println!("  {}", row_line(row));
    }
    println!();

    print_username_facts(&view.rows);
    print_rewrite_summary(&view.rewrites);
    print_typo_summary(&view.typos);

    println!("这只是报告，**什么都没写** —— 没有写注册表、没有落盘、没有广播。");
    println!("要真的改：`tuoen path apply <snapshot> --only <类> [--pick <id>] [--dry-run]`。");
}

/// 用户级那一节的标签 —— 判据是**计划**（`PathPlan::will_write()`），不是"这个作用域可写"。
///
/// 恒印"会写"在 `applied` 为空、`wrote == false` 的形态下是一句**与事实相反的话**：
/// 用户读到的预览里写着"会写"，真跑起来却什么都没写 —— 而下面 [`print_visibility`]
/// 那句"计划与现状一致"只是把它盖住了一半。这正是这张票从头到尾在防的那一类。
///
/// `will_write()` 同时覆盖"条目变了"与"类型要换"（值含 `%` → `EXPAND_SZ`）两种情况，
/// 所以它是唯一不撒谎的判据 —— CLI **不自己重算**（重算就是第二份事实）。
fn user_scope_label(plan_will_write: bool) -> String {
    if plan_will_write {
        "**会写**".to_owned()
    } else {
        "**不会写（计划与现状一致）**".to_owned()
    }
}

/// `tuoen path apply` 的人类输出。
///
/// 顺序是刻意的：**先计划、再预算、再"选了但没做事"、再提权、最后才是写没写**。
/// 用户读完计划之后才有资格判断"要不要真跑"，而长度预算决定的是"能不能跑"。
pub fn print_apply_human(view: &PathApplyView, username_source: &str, plan_will_write: bool) {
    println!("PATH 重建计划 —— 目标快照：{}", view.snapshot);
    println!(
        "{}",
        username_line(view.current_username.as_deref(), username_source)
    );
    println!(
        "选择：类 [{}] · 条目 [{}]",
        view.selection.classes.join(", "),
        view.selection.picks.join(", ")
    );
    if view.dry_run {
        println!("模式：`--dry-run` —— 只报计划，**一个字节都不写**。");
    } else {
        println!("模式：真写（只写用户级；机器级要提权，tuoen 不自动提权）。");
    }
    println!();

    println!("计划（逐作用域）：");
    if view.scopes.is_empty() {
        println!("  （没有任何作用域有内容 —— 这次不会碰注册表。）");
    }
    for scope in &view.scopes {
        let label = scope_label(scope.scope.as_str());
        let before_chars = scope.before_raw.chars().count();
        let after_chars = scope.after_raw.chars().count();
        let will_write = scope.scope == EnvScope::User;
        println!(
            "  [{label}] {before_chars} → {after_chars} 字符；条目 {} → {} 条；未改动 {} 条；{}",
            scope.before_entries.len(),
            scope.after_entries.len(),
            scope.untouched,
            if will_write {
                user_scope_label(plan_will_write)
            } else if scope.requires_elevation {
                "**不会写：机器级要提权**".to_owned()
            } else {
                "**不会写**".to_owned()
            }
        );
        for row in &scope.applied {
            println!("    · {}", applied_line(row));
        }
    }
    println!();

    print_budget(view);
    if view.dry_run && view.budget.level == tuoen_platform::BudgetLevel::Exceeded.as_str() {
        // 预览**照给**（`path apply` 是唯一能产出"重建后的完整列表"的命令），
        // 但"按下去会怎样"必须写在纸上 —— 见 [`too_long_notice`]。
        println!("{}", too_long_notice(&view.budget));
        println!();
    }
    print_no_op_classes(&view.no_op_classes);
    print_elevation(&view.requires_elevation);
    print_visibility(view);
}

/// `当前用户名：X（来自 …）` 那一行。**取不到时不许装作有**。
#[must_use]
pub fn username_line(username: Option<&str>, source: &str) -> String {
    match username {
        Some(name) => format!("当前用户名：{name}（来自{source}）"),
        None => "当前用户名：**取不到**（进程环境与用户级注册表块里都没有 USERPROFILE / \
                 USERNAME）—— 这一轮**一条都不会重写**。"
            .to_owned(),
    }
}

/// 一条 diff 行的人类形态。
fn row_line(row: &PathDiffRow) -> String {
    // 值的来源：本机侧优先（"这条在本机是什么"是用户要看的），只有目标侧的行才用目标值。
    let side = row.local.as_ref().or(row.target.as_ref());
    let value = side.map_or("", |side| side.value.as_str());
    let mut line = format!(
        "[{}] {} {} {}",
        row.id,
        row.class.as_str(),
        row.reason.as_str(),
        value
    );
    if !row.also.is_empty() {
        let also = row
            .also
            .iter()
            .map(|reason| reason.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        line.push_str(&format!("（还同时是：{also}）"));
    }
    if let Some(duplicate_of) = &row.duplicate_of {
        line.push_str(&format!(" —— 与 {duplicate_of} 重复"));
    }
    if let Some(suggestion) = &row.suggestion {
        line.push_str(&format!(" —— 疑似错写 → {suggestion}"));
    }
    if let Some(to) = &row.rewrite_to {
        line.push_str(&format!(" —— 将重写为 {to}"));
    }
    line
}

/// 一条 `applied` 的人类形态。
fn applied_line(row: &AppliedRow) -> String {
    let at = match row.from_index {
        Some(from) => format!("第 {from} 位 → 第 {} 位", row.to_index),
        None => format!("插到第 {} 位", row.to_index),
    };
    format!("[{}] {} {}（{at}）", row.id, row.class.as_str(), row.value)
}

/// **硬编码用户名的条数**：从 `rows[].local.hasUsername` 自己数，机器级 / 用户级分开。
///
/// 为什么必须分开报：本机 12 条里 **2 条在机器级** —— 那两条即使想改也要提权
/// （`docs/DESIGN.md` §2.1 的实测）。合成一个数字会让"换机之后有几条会静默失效"
/// 这个问题少一个答案。
///
/// 为什么**不**按 `fix` 类数：决策 141 之后，"硬编码了用户名"是**事实**，
/// 而"这一类的 `fix` 会重写它"是**动作** —— 名字就是当前用户名时没有旧名可换，
/// 那些行按其余性质归类（`dangling` / `move` / `keep`），事实留在 `also` 里。
fn print_username_facts(rows: &[PathDiffRow]) {
    let count = |scope: EnvScope| {
        rows.iter()
            .filter(|row| {
                row.local
                    .as_ref()
                    .is_some_and(|side| side.has_username && side.scope == scope)
            })
            .count()
    };
    let machine = count(EnvScope::Machine);
    let user = count(EnvScope::User);
    if machine + user == 0 {
        println!("硬编码了用户名的条目：没有（按 `local.hasUsername` 数的）。");
    } else {
        println!(
            "硬编码了用户名的条目：{} 条（机器级 {machine} 条 · 用户级 {user} 条）—— \
             换账号名之后它们会静默失效。",
            machine + user
        );
        println!(
            "  （按 `local.hasUsername` 数的，**不是**按 `fix` 类数的：名字就是当前用户名时\
             没有旧名可换，那些行按其余性质归类，这条事实留在 `also` 里。）"
        );
    }
    println!();
}

/// 用户名重写的条数。逐条的"将重写为"已经印在行上了，这里只给总数 ——
/// 票据要求的是"**必须显式报告重写了哪几条**"，而逐行打印就是那份报告。
fn print_rewrite_summary(rewrites: &[Rewrite]) {
    if rewrites.is_empty() {
        println!("用户名重写：没有（没有「旧名可换」的条目）。");
    } else {
        println!(
            "用户名重写：{} 条（上面每一条都印了「将重写为」）—— 换机后必须改的就是这些。",
            rewrites.len()
        );
    }
    println!();
}

/// 拼写疑似。**只报告，永不纠错**（决策 133）。
fn print_typo_summary(typos: &[TypoSuspect]) {
    if typos.is_empty() {
        println!("拼写疑似：没有。");
    } else {
        println!(
            "拼写疑似：{} 条 —— **只报告，永不自动纠错**（改错一个目录名比不改更危险）。",
            typos.len()
        );
    }
    println!();
}

/// 长度预算。**下界**这个词不许省：它不含进程注入项（本机实测 78 字符）。
fn print_budget(view: &PathApplyView) {
    let before = &view.budget_before;
    let after = &view.budget;
    println!(
        "长度：用户级 {} → {} 字符",
        before.raw_user_chars, after.raw_user_chars
    );
    println!(
        "      {} → {} 字符（**注册表口径的下界**，不含进程注入；档位 {}；还剩 {} 字符；\
         悬崖 {}）",
        before.effective_chars, after.effective_chars, after.level, after.remaining, after.cliff
    );
    println!(
        "      （机器级原文 {} 字符。超过悬崖之后 `cmd.exe` 会**完全忽略整条 `PATH`** —— \
         不是少几条，是所有命令一起失效。）",
        after.raw_machine_chars
    );
    println!();
}

/// `exceeded` + `--dry-run` 时必须说出来的那一句（Lead 的补充裁决）。
///
/// **预览照给，但"按下去会怎样"必须写在纸上。** 一个 `PATH` 已经 9000 字符的用户，
/// 唯一的自救信息就是这份重建结果（`path diff` 只给逐条分类，不给新列表）—— 把预览
/// 也拒掉，他就只能盲删。反过来说：预览成功而真写拒绝，**必须**在预览里说清楚，
/// 否则"`--dry-run` 通过了"会被读成"能写"。
///
/// 这一句同时进人类输出（`print_apply_human`）与 `--json` 模式的 **stderr**
/// （`path apply` 的 JSON 键由 core 拥有，多一个键就是第二份事实）。
pub fn too_long_notice(budget: &BudgetRowView) -> String {
    let over = budget.effective_chars.saturating_sub(budget.cliff);
    format!(
        "**注意：档位 `exceeded`（{} 字符 > 悬崖 {}，超了 {over} 字符）—— \
         真写会被拒绝（`too-long`）。** 要写进去，得先把重建后的原文至少降 {over} 字符。",
        budget.effective_chars, budget.cliff
    )
}

/// **被选中、但没有任何变换可做**的类（决策 144）。
///
/// 不打印它，"`--only case-only` 拿到一份与输入逐字相同的 `PATH` 且没有任何解释"就会
/// 变成"看起来在工作"。判据由 core 给（`Rebuild.no_op_classes`），CLI **不自己记规则**。
fn print_no_op_classes(classes: &[DiffClass]) {
    if classes.is_empty() {
        return;
    }
    for class in classes {
        println!(
            "你选了 `{}`，但这一类**不做任何改写**：{}。",
            class.as_str(),
            no_op_reason(*class)
        );
    }
    println!();
}

/// 一个类"不做改写"的原因。**人话，只在人类输出里**。
const fn no_op_reason(class: DiffClass) -> &'static str {
    match class {
        DiffClass::CaseOnly => "Windows 上同一个目录，改大小写零收益、纯风险（决策 128）",
        DiffClass::Keep => "`keep` 本来就是「两边一样」，没有可改的东西",
        DiffClass::Add => "目标里没有一条是本机缺的（或者那些 `add` 已经在 `PATH` 上了）",
        DiffClass::Remove => "本机没有一条归 `remove`（目标里没有的，本机也一条都没有）",
        DiffClass::Move => "没有一条的位置需要挪（相对顺序已经与目标一致）",
        DiffClass::Fix => "没有一条需要修（空条目 / 重复 / 失效 / 旧用户名都没有命中）",
    }
}

/// 机器级的改动：**算出来了，但不写**。
fn print_elevation(rows: &[ElevationRow]) {
    if rows.is_empty() {
        println!("机器级：这次没有任何改动要写（所以不需要提权）。");
        println!();
        return;
    }
    println!(
        "以下 {} 条改动在**机器级**，需要提权；**tuoen 不自动提权**（决策 12/136）：",
        rows.len()
    );
    for row in rows {
        println!("  · [{}] {} {}", row.id, row.class.as_str(), row.value);
    }
    println!(
        "  （机器级的结果**已经算出来了**（上面那一节的 `afterRaw` 是完整的），\
         你可以拿它去别处用，或等 `restore`。）"
    );
    println!();
}

/// 写没写 + 可见性说明（决策 138）。**两段话一个字都不许省**。
///
/// 用户看不到这两段时，最可能的反应是"改了没生效，这工具有 bug" ——
/// 而这一票的每一次真实写入都会遇到它。所以 `--dry-run` 里也要印：
/// 它讲的正是"写下去之后会怎样"。
fn print_visibility(view: &PathApplyView) {
    if view.dry_run {
        println!("以上是计划，**什么都没有写** —— 没有写注册表、没有广播。");
        println!("（真跑时会是这样：）");
    } else if view.wrote {
        println!(
            "✓ 已写入用户级 PATH：{} 字符，类型 {}",
            view.chars, view.reg_type
        );
        println!(
            "广播回执：{} 个顶层窗口应答（它只证明广播发出去了，**不证明所有进程都更新了**）。",
            view.broadcast_replies
        );
    } else {
        println!("计划与现状一致 —— **没有写、也没有广播**（空操作是成功，不是失败）。");
    }
    println!(
        "**已经在跑的**终端 / IDE / 服务 / 计划任务拿不到新环境：环境块是 `CreateProcess` 时\
         复制的，**我们自己的进程也拿不到**。"
    );
    println!(
        "所以「**请重启终端**」是平台限制，不是工具不友好 —— 环境块只在进程创建时复制一次，\
         之后谁也改不进去。"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_core::pathdiff::DiffReason;

    fn a_row(class: DiffClass, reason: DiffReason) -> PathDiffRow {
        PathDiffRow {
            id: "user:0".to_owned(),
            class,
            reason,
            also: Vec::new(),
            target: None,
            local: None,
            duplicate_of: None,
            rewrite_to: None,
            suggestion: None,
        }
    }

    #[test]
    fn the_visibility_slug_is_the_machine_readable_form_of_decision_138() {
        assert_eq!(VISIBILITY_RESTART_REQUIRED, "restart-required");
        assert!(
            VISIBILITY_RESTART_REQUIRED
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'),
            "稳定 slug 必须是小写 kebab ASCII"
        );
    }

    #[test]
    fn the_budget_view_renames_the_disk_model_keys() {
        // 磁盘模型是 snake_case（它同时要当 TOML 的键），`--json` 是 camelCase ——
        // 验收脚本读的是 `rawUserChars`。这条用例钉住那次改名确实发生了。
        let row = PathBudgetRow {
            raw_user_chars: 773,
            raw_machine_chars: 1007,
            effective_chars: 1780,
            cliff: 8191,
            remaining: 6411,
            level: "ok".to_owned(),
        };
        let json = serde_json::to_string(&BudgetRowView::from(&row)).expect("序列化");
        assert_eq!(
            json,
            r#"{"rawUserChars":773,"rawMachineChars":1007,"effectiveChars":1780,"cliff":8191,"remaining":6411,"level":"ok"}"#
        );
        assert!(!json.contains("raw_user_chars"), "{json}");
    }

    #[test]
    fn a_diff_row_prints_its_rewrite_and_its_suggestion_but_never_acts_on_them() {
        let mut row = a_row(DiffClass::Fix, DiffReason::UsernameHardcoded);
        row.rewrite_to = Some(r"C:\Users\new\bin".to_owned());
        let line = row_line(&row);
        assert!(line.contains("将重写为"), "{line}");
        assert!(line.contains(r"C:\Users\new\bin"), "{line}");

        let mut row = a_row(DiffClass::Fix, DiffReason::Dangling);
        row.suggestion = Some(r"C:\Software\tools".to_owned());
        let line = row_line(&row);
        assert!(line.contains("疑似错写 →"), "{line}");
        assert!(line.contains(r"C:\Software\tools"), "{line}");
    }

    #[test]
    fn every_class_has_a_no_op_explanation_and_case_only_names_the_decision() {
        // 六个类都要有一句人话 —— 少一个会让"你选了 X，但这一类不做任何改写："
        // 后面跟着一句空话。
        for class in DiffClass::ALL {
            let reason = no_op_reason(class);
            assert!(!reason.is_empty(), "{} 没有解释", class.as_str());
        }
        assert!(no_op_reason(DiffClass::CaseOnly).contains("决策 128"));
    }

    #[test]
    fn a_missing_username_is_said_out_loud() {
        // `null` 与"用户名恰好就是当前用户名"在输出上长得一样，所以取不到时必须说出来。
        let line = username_line(None, "进程环境 USERPROFILE");
        assert!(line.contains("取不到"), "{line}");
        assert!(line.contains("不会重写"), "{line}");
        let line = username_line(Some("Muelsyse"), "进程环境 USERPROFILE");
        assert!(line.contains("Muelsyse"), "{line}");
        assert!(
            line.contains("进程环境 USERPROFILE"),
            "来源必须说出来：{line}"
        );
    }
}
