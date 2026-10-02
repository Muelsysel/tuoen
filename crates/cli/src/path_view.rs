//! `tuoen path` 的 **`--json` 形状**与**人类输出**。
//!
//! 与 [`crate::manage_view`]、[`crate::shim_view`] 分开：那一族描述的是
//! "装了什么 / 刚发生了什么"，这一族描述的是 **`PATH` 上现在有什么、
//! 以及一次改动会把它变成什么**。
//!
//! ## 两条硬规矩
//!
//! 1. **`--json` 的成功载荷里不出现中文**（决策 35）。所以每一个结论性的字段都是
//!    **稳定 slug**：预算是 `ok` / `warning` / `critical` / `exceeded`，一次改动是
//!    `add` / `remove` / `noop` / `retype` / `drop-empty-segments`，空操作的原因
//!    （由 `tuoen_platform::NoopReason` 的 `Serialize` 给出，见本文件底部的测试）
//!    是 `already-present` / `absent` / `protected-shim-dir`。中文只在**人类输出**里。
//! 2. **人类输出是中文**，而且要说清"为什么"，不只是"是什么" ——
//!    尤其是"用户级永远排在机器级后面"与"8191 是整条失效、1024 是静默裁剪"这两件
//!    光看数字看不出来的事。
//!
//! ## 为什么 `--json` 里允许出现路径文本（可能是非 ASCII）
//!
//! `serde_json` 把非 ASCII 转义成 `\uXXXX`，所以**输出永远是纯 ASCII**，
//! 逐字节稳定这一条不受影响。而"哪一条、在哪一个作用域、第几段"正是这份报告
//! 的全部价值 —— 去掉路径它就只剩下一堆计数了。
//!
//! ## 为什么 `changes` 直接复用平台的 `PathChange`
//!
//! 那是**同一个东西**的两端：CLI 是它唯一的消费者，而它的 `Serialize` 形状
//! （`tag = "kind"`，kebab-case）就是契约本身。在这里再定义一遍 `ChangeView`
//! 等于把同一张表写两份，而两份必然会漂移。取值由本文件底部的测试钉住。

use serde::Serialize;
use tuoen_platform::{EnvScope, PathAnalysis, PathBudget, PathChange, PathPlan, RegType};

use crate::display_width;

/// 两个路径说的是不是同一个目录 —— 大小写不敏感、结尾反斜杠不算差别。
///
/// 用它而不是 `==`：用户敲的是 `tuoen path add C:\Users\x\AppData\Local\tuoen\shims\`，
/// 而我们从 store 拼出来的那份没有结尾反斜杠、大小写也可能不同。字面比较会让
/// "这一次加的就是我们的 shim 目录"判断不出来，于是那段最该印的报告反而不印。
fn same_path(left: &str, right: &str) -> bool {
    fn trim(value: &str) -> String {
        let value = value.trim();
        let stripped = value.trim_end_matches(['\\', '/']);
        // 盘根（`C:\`）削完会变成 `C:`，补回来；否则 `C:\` 与 `C:` 会被当成同一个。
        if stripped.ends_with(':') {
            format!("{stripped}\\")
        } else {
            stripped.to_owned()
        }
    }
    trim(left).eq_ignore_ascii_case(&trim(right))
}

/// 我们要读写的那个环境变量名。**与平台层的定义必须一致。**
///
/// 值取自 [`tuoen_platform::path::PATH_NAME`]（`crate::path_view` 底部的测试
/// 在编译期钉住这件事），而不是 `tuoen_platform` 的 crate 根 —— 那个名字只在
/// `path` 模块里导出，重导出它属于平台层的改动，而这一票**不许动平台层**。
const PATH_NAME: &str = "Path";

// ─────────────────────────────────────────────────────────────────────────────
// show
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen path show --json` 的载荷。
///
/// 顶层是**计数**，每一类问题同时给出计数与明细：计数让 GUI / 脚本不用遍历数组
/// 就能画出概览，明细让"到底是哪几条"可查。两个都要，因为这两件事的消费者不同。
#[derive(Debug, Serialize)]
pub struct PathShowView {
    /// 被分析的命令名：**永远是 `Path`**（见本文件的 [`PATH_NAME`]）。
    pub name: &'static str,
    /// 长度预算。**以生效的 `PATH` 为准**，不是以某一个作用域为准。
    pub budget: BudgetView,
    /// 两个作用域，**按生效顺序**：[0] 是机器级，[1] 是用户级。
    pub scopes: Vec<ScopeView>,
    /// **真实的生效顺序**，逐条标注它来自哪个作用域（`machine` / `user` /
    /// `process-only`）。
    ///
    /// 以**进程里那一条 `PATH`** 为准 —— 那才是 Windows 真正拼出来的东西。
    /// 本机实测的形状是「注入项 + 机器级 + 用户级」：**注入项排在机器级前面**，
    /// 所以"机器级在前、用户级在后"只是这个顺序的一部分，不是它的全部。
    /// 注册表里有、进程里没有的（这个终端在改动之前就开了）追加在末尾。
    ///
    /// **谁赢名字冲突由这个顺序说了算**，不是由 `scopes` 说了算。
    pub effective: Vec<EntryView>,
    /// 机器级条目数（**不含**空段）。
    #[serde(rename = "machineEntries")]
    pub machine_entries: usize,
    /// 用户级条目数（**不含**空段）。
    #[serde(rename = "userEntries")]
    pub user_entries: usize,
    /// 进程 `PATH` 里有、两个作用域都没有的条目 —— **进程注入项**。
    /// 它们不在任何注册表里，所以只看注册表会漏掉它们，而它们真的占长度。
    #[serde(rename = "processOnly")]
    pub process_only: Vec<String>,
    #[serde(rename = "processOnlyCount")]
    pub process_only_count: usize,
    pub duplicates: Vec<DuplicateView>,
    #[serde(rename = "duplicateCount")]
    pub duplicate_count: usize,
    /// 重复里**多出来的**段数（`Σ(次数 − 1)`）—— 这是"清掉能省多少条"的答案。
    #[serde(rename = "duplicateExtraSegments")]
    pub duplicate_extra_segments: usize,
    pub dangling: Vec<DanglingView>,
    #[serde(rename = "danglingCount")]
    pub dangling_count: usize,
    /// 硬编码了用户名的条目。
    #[serde(rename = "usernameDependencies")]
    pub username_dependencies: Vec<UsernameView>,
    #[serde(rename = "usernameDependencyCount")]
    pub username_dependency_count: usize,
    /// 其中**在机器级**的那些 —— 最危险的一类：换账号名之后静默失效。
    #[serde(rename = "usernameDependenciesAtMachineScope")]
    pub username_dependencies_at_machine_scope: usize,
    #[serde(rename = "shadowedShims")]
    pub shadowed_shims: Vec<ShadowedView>,
    #[serde(rename = "shadowedShimCount")]
    pub shadowed_shim_count: usize,
    /// 我们自己的 shim 目录；没给（或读不到）时是 `null`。
    #[serde(rename = "shimDir")]
    pub shim_dir: Option<String>,
    /// 我们的 shim 目录里发布了哪些命令（已排序）。**空数组的含义是"一条都还没有"。**
    ///
    /// 它是 [`Self::shadowed_shims`] 的**分母**：没有分母的时候"零条被遮蔽"
    /// 是个空洞的结论 —— 报告必须说"没得比"，而不是说"都没输"。
    #[serde(rename = "shimCommands")]
    pub shim_commands: Vec<String>,
}

impl PathShowView {
    /// 从一次分析的结果构造。`shim_dir` 与 `analysis` 必须来自**同一次**分析，
    /// 否则报告里会出现"遮蔽是按另一个目录算的"这种自相矛盾。
    #[must_use]
    pub fn new(analysis: &PathAnalysis, shim_dir: Option<&str>) -> Self {
        let scope_entries = |wanted: EnvScope| {
            analysis
                .scopes
                .iter()
                .filter(|scope| scope.scope == wanted)
                .map(|scope| {
                    scope
                        .entries
                        .iter()
                        .filter(|entry| !entry.is_empty())
                        .count()
                })
                .sum()
        };
        Self {
            name: PATH_NAME,
            budget: BudgetView::from(&analysis.budget),
            scopes: analysis.scopes.iter().map(ScopeView::from).collect(),
            effective: analysis.effective.iter().map(EntryView::from).collect(),
            machine_entries: scope_entries(EnvScope::Machine),
            user_entries: scope_entries(EnvScope::User),
            process_only_count: analysis.process_only.len(),
            process_only: analysis.process_only.clone(),
            duplicate_count: analysis.duplicates.len(),
            duplicate_extra_segments: analysis
                .duplicates
                .iter()
                .map(|duplicate| duplicate.at.len().saturating_sub(1))
                .sum(),
            duplicates: analysis
                .duplicates
                .iter()
                .map(DuplicateView::from)
                .collect(),
            dangling_count: analysis.dangling.len(),
            dangling: analysis.dangling.iter().map(DanglingView::from).collect(),
            username_dependency_count: analysis.username_dependencies.len(),
            username_dependencies_at_machine_scope: analysis
                .username_dependencies
                .iter()
                .filter(|dependency| dependency.at_machine_scope)
                .count(),
            username_dependencies: analysis
                .username_dependencies
                .iter()
                .map(UsernameView::from)
                .collect(),
            shadowed_shim_count: analysis.shadowed_shims.len(),
            shadowed_shims: analysis
                .shadowed_shims
                .iter()
                .map(ShadowedView::from)
                .collect(),
            shim_dir: shim_dir.map(ToOwned::to_owned),
            shim_commands: analysis.shim_commands.clone(),
        }
    }

    /// 一句话结论（**中文**，只进人类输出）。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "生效 PATH {} 字符，离悬崖还有 {} 字符（档位 {}）。",
            self.budget.effective_chars, self.budget.remaining, self.budget.level
        )
    }
}

/// 长度预算。
#[derive(Debug, Serialize)]
pub struct BudgetView {
    /// 生效 `PATH` 的字符数（进程环境块里那一条算出来的）。
    #[serde(rename = "effectiveChars")]
    pub effective_chars: usize,
    /// 机器级 + 用户级注册表原文的字符数。**与 `effectiveChars` 可能不等**。
    #[serde(rename = "registryChars")]
    pub registry_chars: usize,
    /// 悬崖（`cmd.exe` 超过它就完全忽略整条 `PATH`）。
    pub cliff: usize,
    /// 还剩多少字符（`exceeded` 时是 0，不做回绕）。
    pub remaining: usize,
    /// 稳定 slug：`ok` / `warning` / `critical` / `exceeded`。
    pub level: &'static str,
    /// 是不是需要显眼告警的那两档。
    pub alarming: bool,
}

impl From<&PathBudget> for BudgetView {
    fn from(budget: &PathBudget) -> Self {
        Self {
            effective_chars: budget.effective_chars,
            registry_chars: budget.registry_chars,
            cliff: budget.cliff,
            remaining: budget.remaining,
            level: budget.level.as_str(),
            alarming: budget.level.is_alarming(),
        }
    }
}

/// 一个作用域里的 `PATH`。
#[derive(Debug, Serialize)]
pub struct ScopeView {
    /// 稳定 slug：`machine` / `user`（`EnvScope::as_str`，**唯一**一份定义）。
    pub scope: &'static str,
    /// 注册表里的原文（**未展开**）。
    pub raw: String,
    /// 注册表里的类型；这个作用域里没有 `Path` 时是 `null`。
    #[serde(rename = "regType")]
    pub reg_type: Option<&'static str>,
    /// **包含空段**的条目序列 —— `index` 就是这条在原文里的第几段，
    /// 所以这里的下标与 `duplicates[].at[].index` 是同一套坐标。
    pub entries: Vec<SegmentView>,
    /// 非空条目数。
    pub count: usize,
    /// `raw` 的字符数。
    pub chars: usize,
    /// 空段在条目序列里的位置。
    #[serde(rename = "emptyPositions")]
    pub empty_positions: Vec<usize>,
}

impl From<&tuoen_platform::ScopedPath> for ScopeView {
    fn from(scope: &tuoen_platform::ScopedPath) -> Self {
        // 下标在**这里**数，而不是在每一条自己的 `From` 里 —— 一段不知道自己是第几段，
        // 而 `index` 必须与 `duplicates[].at[].index` 是同一套坐标（它们都指第几段）。
        let entries = scope
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| SegmentView {
                index,
                raw: entry.raw.clone(),
                value: entry.value.clone(),
                quoted: entry.quoted,
                empty: entry.is_empty(),
            })
            .collect();
        Self {
            scope: scope.scope.as_str(),
            raw: scope.raw.clone(),
            reg_type: scope.reg_type.as_ref().map(reg_type_slug),
            entries,
            count: scope
                .entries
                .iter()
                .filter(|entry| !entry.is_empty())
                .count(),
            chars: scope.chars,
            empty_positions: scope.empty_positions.clone(),
        }
    }
}

/// `PATH` 里的一段。
#[derive(Debug, Serialize)]
pub struct SegmentView {
    /// 在该作用域的条目序列里的下标（**含空段**，所以它就是原文里的第几段）。
    pub index: usize,
    /// 原文，**原样保留**（含引号与可能的首尾空白）。
    pub raw: String,
    /// 用于比较的规范形态。
    pub value: String,
    /// 原文是不是被一对引号包着。**引号保护的是空格，不是 `;`**。
    pub quoted: bool,
    /// 空段（`;;`、结尾的 `;`）。**这是真实存在的**，本机机器级里就有。
    pub empty: bool,
}

impl From<&tuoen_platform::PathEntry> for SegmentView {
    fn from(entry: &tuoen_platform::PathEntry) -> Self {
        // 单独的 `PathEntry` 不知道自己在这个作用域里排第几，所以这里只能是 0。
        // 带下标的构造在 [`ScopeView`] 的 `From` 里 —— 那才是唯一正确的地方。
        Self {
            index: 0,
            raw: entry.raw.clone(),
            value: entry.value.clone(),
            quoted: entry.quoted,
            empty: entry.is_empty(),
        }
    }
}

/// 指向某个作用域里的某一段。
#[derive(Debug, Serialize)]
pub struct EntryView {
    /// 稳定 slug：`machine` / `user`。
    pub scope: &'static str,
    /// 在该作用域条目序列里的下标（含空段）。
    pub index: usize,
    pub raw: String,
    pub value: String,
}

impl From<&tuoen_platform::EntryRef> for EntryView {
    fn from(entry: &tuoen_platform::EntryRef) -> Self {
        Self {
            scope: entry.scope.as_str(),
            index: entry.index,
            raw: entry.raw.clone(),
            value: entry.value.clone(),
        }
    }
}

/// 同一个目录出现了多次。
#[derive(Debug, Serialize)]
pub struct DuplicateView {
    /// 大小写折叠后的值，作为分组键。
    pub key: String,
    /// 出现了几次。
    pub count: usize,
    /// 按生效顺序排列的每一次出现。
    pub at: Vec<EntryView>,
}

impl From<&tuoen_platform::Duplicate> for DuplicateView {
    fn from(duplicate: &tuoen_platform::Duplicate) -> Self {
        Self {
            key: duplicate.key.clone(),
            count: duplicate.at.len(),
            at: duplicate.at.iter().map(EntryView::from).collect(),
        }
    }
}

/// 指向一个不存在的目录。
#[derive(Debug, Serialize)]
pub struct DanglingView {
    pub entry: EntryView,
    /// 这条是不是要展开变量才成立（含 `%`）。平台层**不展开**它，
    /// 所以命中的这一条理论上可能是假阳性 —— 这个字段就是留给那种情况的。
    #[serde(rename = "usesVariable")]
    pub uses_variable: bool,
}

impl From<&tuoen_platform::DanglingEntry> for DanglingView {
    fn from(dangling: &tuoen_platform::DanglingEntry) -> Self {
        Self {
            entry: EntryView::from(&dangling.entry),
            uses_variable: dangling.uses_variable,
        }
    }
}

/// 一条硬编码了用户名的条目。
#[derive(Debug, Serialize)]
pub struct UsernameView {
    pub entry: EntryView,
    /// 被硬编码进去的用户名。
    pub name: String,
    /// **这条在机器级**吗。机器级的硬编码用户名最危险：换账号名之后它静默失效。
    #[serde(rename = "atMachineScope")]
    pub at_machine_scope: bool,
}

impl From<&tuoen_platform::UsernameDependency> for UsernameView {
    fn from(dependency: &tuoen_platform::UsernameDependency) -> Self {
        Self {
            entry: EntryView::from(&dependency.entry),
            name: dependency.name.clone(),
            at_machine_scope: dependency.at_machine_scope,
        }
    }
}

/// 我们发布的一条命令被别的条目抢在了前面。
#[derive(Debug, Serialize)]
pub struct ShadowedView {
    /// 命令名（不含扩展名），例如 `node`。
    pub command: String,
    /// 抢在我们前面的那一段。
    pub by: EntryView,
    /// 那条目录里实际命中的文件（含扩展名）。
    pub file: String,
    /// 我们自己的 shim 目录。
    #[serde(rename = "shimDir")]
    pub shim_dir: String,
}

impl From<&tuoen_platform::ShadowedShim> for ShadowedView {
    fn from(shadowed: &tuoen_platform::ShadowedShim) -> Self {
        Self {
            command: shadowed.command.clone(),
            by: EntryView::from(&shadowed.by),
            file: shadowed.file.clone(),
            shim_dir: shadowed.shim_dir.clone(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// add / remove 的计划
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen path add|remove --json` 的载荷。
///
/// **`--dry-run` 与真写是同一个形状**，区别只在 `dryRun` / `willWrite` / `applied`
/// 这三个字段。给演练单独一套形状会让消费者写两套解析，而"我只想看会做什么"与
/// "我真做了"要读的字段其实是同一批（决策 20）。
#[derive(Debug, Serialize)]
pub struct PathPlanView {
    /// `add` 或 `remove` —— **不本地化**。
    pub action: &'static str,
    /// 用户给的目录，**原样**。
    pub dir: String,
    /// 这个计划会改哪一个作用域。**永远是 `user`**：机器级要提权。
    pub scope: &'static str,
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
    /// 这一步会不会真的写注册表。**空操作时是 `false`，而 `false` 时一个字节都不写。**
    #[serde(rename = "willWrite")]
    pub will_write: bool,
    /// 改之前的原文。
    #[serde(rename = "beforeRaw")]
    pub before_raw: String,
    /// 改之后的原文。
    #[serde(rename = "afterRaw")]
    pub after_raw: String,
    /// 值会不会真的变。
    #[serde(rename = "changesValue")]
    pub changes_value: bool,
    #[serde(rename = "beforeType")]
    pub before_type: &'static str,
    #[serde(rename = "afterType")]
    pub after_type: &'static str,
    /// 具体改了哪几段。**取值是稳定 slug**（`PathChange` 的 `kind`）。
    pub changes: Vec<PathChange>,
    /// 写回之后的长度预算。**注册表口径的下界**：它不含进程注入的那些条目，
    /// 所以真实的生效长度只会更长、只会更早撞悬崖。
    #[serde(rename = "budgetAfter")]
    pub budget_after: BudgetView,
    /// 真的做了什么（`--dry-run`、或者计划是空操作时是 `null`）。
    pub applied: Option<AppliedView>,
    /// 我们发布 shim 的那个目录。**`null` 的含义是"这次没有查遮蔽"**。
    ///
    /// 与 `shadowedShims: []` 必须分得开：后者是"查了，一条都没被抢"。
    /// 这两件事在报告里长得一样，而它们的意思正相反（决策 73）。
    #[serde(rename = "shimDir")]
    pub shim_dir: Option<String>,
    /// 做完这一步之后**仍然**被别的目录抢在前面的命令。
    ///
    /// 它报告的是"写完之后重新分析"的结果：`path add` 只把我们的目录**追加到末尾**，
    /// 所以它**永远修不好遮蔽** —— 名字冲突永远是先命中的赢。报告就是这件事的兜底：
    /// 与其让用户以为加完 PATH 就完事了，不如当场告诉他还有几条没赢。
    #[serde(rename = "shadowedShims")]
    pub shadowed_shims: Vec<ShadowedView>,
    #[serde(rename = "shadowedShimCount")]
    pub shadowed_shim_count: usize,
    /// 我们的 shim 目录里发布了哪些命令（`<名字>`，已排序）。
    ///
    /// **它是 [`Self::shadowed_shims`] 的分母**：空数组时那条列表必然为空，
    /// 但那不是"没输"，是"没得比" —— 报告必须把这两件事分开说。
    #[serde(rename = "shimCommands")]
    pub shim_commands: Vec<String>,
}

/// 真的写了什么。**只在 `apply` 真的落盘之后才有。**
#[derive(Debug, Serialize)]
pub struct AppliedView {
    pub wrote: bool,
    #[serde(rename = "regType")]
    pub reg_type: &'static str,
    pub chars: usize,
    /// 广播回执：收到了几个顶层窗口的应答。**它只是"广播发出去了"的证据，
    /// 不是"所有进程都更新了"的证据** —— 已经跑着的进程永远拿不到新环境。
    #[serde(rename = "broadcastReplies")]
    pub broadcast_replies: usize,
}

impl PathPlanView {
    /// 从平台的计划 + （真写时的）落盘结果构造。
    #[must_use]
    pub fn new(
        action: &'static str,
        dir: &str,
        plan: &PathPlan,
        dry_run: bool,
        applied: Option<&tuoen_platform::PathApplied>,
    ) -> Self {
        Self {
            action,
            dir: dir.to_owned(),
            scope: plan.scope.as_str(),
            dry_run,
            will_write: plan.will_write(),
            before_raw: plan.before_raw.clone(),
            after_raw: plan.after_raw.clone(),
            changes_value: plan.changes_value,
            before_type: reg_type_slug(&plan.before_type),
            after_type: reg_type_slug(&plan.after_type),
            changes: plan.changes.clone(),
            budget_after: BudgetView::from(&plan.budget_after),
            applied: applied.map(AppliedView::from),
            shim_dir: None,
            shadowed_shims: Vec::new(),
            shadowed_shim_count: 0,
            shim_commands: Vec::new(),
        }
    }

    /// 补上"写完之后重新分析"的遮蔽现状。
    ///
    /// **必须在 [`Self::new`] 之后重新跑一次 [`tuoen_platform::analyze`]**，而不是把
    /// 计划之前那次分析的结果塞进来：`--dry-run` 时机器没变、两者相同，但真写之后
    /// 注册表变了，拿旧结果就是在报告一个已经不存在的世界（决策 73）。
    #[must_use]
    pub fn with_shadowing(
        mut self,
        analysis: &tuoen_platform::PathAnalysis,
        shim_dir: Option<&std::path::Path>,
    ) -> Self {
        self.shim_dir = shim_dir.map(|dir| dir.to_string_lossy().into_owned());
        self.shadowed_shims = analysis
            .shadowed_shims
            .iter()
            .map(ShadowedView::from)
            .collect();
        self.shadowed_shim_count = self.shadowed_shims.len();
        self.shim_commands = analysis.shim_commands.clone();
        self
    }

    /// 遮蔽这一段值不值得印。
    ///
    /// 两种值得：①真被抢了（**必须说**，否则用户以为装完就完事）；
    /// ②我们就在给 shim 目录加 PATH（这一次的用户意图正是"让 tuoen 的命令能用"），
    /// 那"一条都没被抢"是个他等着听的好消息。
    #[must_use]
    pub fn shadow_report_worth_printing(&self) -> bool {
        self.shadowed_shim_count > 0
            || self
                .shim_dir
                .as_deref()
                .is_some_and(|shim| same_path(&self.dir, shim))
    }

    /// 这个计划是**拒绝**而不是空操作吗。
    ///
    /// 现在只有一种拒绝：拿我们自己的 shim 目录去 `remove`。它与"本来就不在里面"
    /// 是**两种不同的结果**（一个是失败、一个是成功的空操作），所以必须分得开。
    #[must_use]
    pub fn refusal(&self) -> Option<&'static str> {
        self.changes.iter().find_map(noop_refusal)
    }

    /// 空操作的中文说明（**只进人类输出**）。
    #[must_use]
    pub fn noop_message(&self) -> Option<String> {
        self.changes.iter().find_map(|change| match change {
            PathChange::Noop { value, reason } => Some(format!(
                "`{value}`：{}",
                noop_reason_human(*reason, &self.dir)
            )),
            _ => None,
        })
    }
}

impl From<&tuoen_platform::PathApplied> for AppliedView {
    fn from(applied: &tuoen_platform::PathApplied) -> Self {
        Self {
            wrote: applied.wrote,
            reg_type: reg_type_slug(&applied.reg_type),
            chars: applied.chars,
            broadcast_replies: applied.broadcast_replies,
        }
    }
}

/// `RegType` → 稳定 slug（**不本地化**）。
///
/// 与 `EnvScope::as_str` / `BudgetLevel::as_str` 同一套取值风格：小写 kebab ASCII。
#[must_use]
pub const fn reg_type_slug(reg_type: &RegType) -> &'static str {
    match reg_type {
        RegType::Sz => "sz",
        RegType::ExpandSz => "expand-sz",
    }
}

/// 一条 `Noop` 是**拒绝**吗（是的话给出稳定错误码）。
///
/// `AlreadyPresent` / `Absent` 都是**成功的空操作** —— "你要的东西已经在 / 不在"
/// 是一个正常的回答。只有 `ProtectedShimDir` 是"我不给你做这件事"。
#[must_use]
pub const fn noop_refusal(change: &PathChange) -> Option<&'static str> {
    match change {
        PathChange::Noop {
            reason: tuoen_platform::NoopReason::ProtectedShimDir,
            ..
        } => Some("protected-shim-dir"),
        _ => None,
    }
}

/// 一条 `Noop` 的中文解释。
#[must_use]
pub fn noop_reason_human(reason: tuoen_platform::NoopReason, dir: &str) -> String {
    match reason {
        tuoen_platform::NoopReason::AlreadyPresent => {
            "已经在用户级 `PATH` 里了（按大小写不敏感比较），**什么都没有改**。".to_owned()
        }
        tuoen_platform::NoopReason::Absent => {
            "本来就不在用户级 `PATH` 里，**什么都没有改**。".to_owned()
        }
        tuoen_platform::NoopReason::ProtectedShimDir => format!(
            "`{dir}` 是 tuoen 自己的 shim 目录，**不许从 `PATH` 上摘掉**。\n\
             摘掉它等于让刚装好的命令全部消失（shim 抢名字靠的就是它在 `PATH` 上）。\n\
             真要去掉：先想清楚那些命令以后从哪里来，然后自己手动改环境变量 —— \
             tuoen 不会替你做这件事。"
        ),
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen path show` 的人类输出。
pub fn print_show_human(view: &PathShowView) {
    println!("{}", view.summary());
    println!(
        "  生效 {} 字符（进程环境块，含进程注入）／注册表 {} 字符（机器级 + 用户级原文）",
        view.budget.effective_chars, view.budget.registry_chars
    );
    println!(
        "  悬崖 {} 字符：`cmd.exe` 超过它就**完全忽略整条 `PATH`** —— \
         不是部分失效，是所有命令一起失效。",
        view.budget.cliff
    );
    println!("  （`setx` 的 1024 是**另一个**悬崖：那个是静默裁剪。tuoen 绝不调用 `setx`。）");
    if view.budget.alarming {
        println!(
            "⚠ 已经用掉 {}% —— 再往里加目录要非常小心。",
            view.budget.effective_chars * 100 / view.budget.cliff.max(1)
        );
    }
    println!();

    for scope in &view.scopes {
        println!(
            "{}：{} 条（{} 字符{}）",
            scope_label(scope.scope),
            scope.count,
            scope.chars,
            if scope.reg_type.is_some() {
                String::new()
            } else {
                " —— 这个作用域里**没有** `Path`".to_owned()
            }
        );
        if !scope.empty_positions.is_empty() {
            println!(
                "  {} 个空段（`;;` 或结尾的 `;`）—— 它们占着位置，也被人算进长度里",
                scope.empty_positions.len()
            );
        }
    }
    println!("（机器级在前、用户级在后 —— 这就是用户级工具永远输掉名字冲突的原因。）");
    println!();

    // 生效顺序里只要出现 `process-only`，就说明"机器级在前"这句话不完整：
    // 注入项排在**机器级前面**，所以它抢名字比机器级还早。
    let injected_first = view
        .effective
        .iter()
        .take_while(|entry| entry.scope == "process-only")
        .count();
    if injected_first > 0 {
        println!(
            "⚠ 生效顺序最前面有 {injected_first} 条**进程注入项**（不在任何注册表里）—— \
             它们抢名字比机器级还早。"
        );
        println!("  所以「机器级在前、用户级在后」只是这个顺序的一部分，不是它的全部。");
        println!();
    }
    println!(
        "生效顺序（{} 条 —— 谁赢名字冲突由它说了算）：{}",
        view.effective.len(),
        effective_preview(&view.effective)
    );
    println!(
        "  （同一个目录在生效顺序里只算第一次出现：第二次没有意义，第一次就已经赢了。\
         所以这里的条数会比「机器级 + 用户级」少，差额就是上面那些重复段。）"
    );
    println!();

    if !view.process_only.is_empty() {
        println!(
            "进程注入项 {} 条（**不在任何注册表里**，所以只看注册表会漏掉它们，而它们真的占长度）：",
            view.process_only_count
        );
        for value in &view.process_only {
            println!("  · {value}");
        }
        println!();
    }

    if view.duplicates.is_empty() {
        println!("重复：没有。");
    } else {
        println!(
            "重复 {} 组（多出来 {} 段 —— 清掉能省这么多，但 tuoen **不会自动清**）：",
            view.duplicate_count, view.duplicate_extra_segments
        );
        for duplicate in &view.duplicates {
            println!("  · {} —— {} 次", duplicate.key, duplicate.count);
            for at in &duplicate.at {
                println!("      {}", entry_label(at.scope, at.index, &at.raw));
            }
        }
    }
    println!();

    if view.dangling.is_empty() {
        println!("失效条目：没有。");
    } else {
        println!(
            "失效条目 {} 条（指向不存在的目录；含 `%` 的条目**不判失效** —— 我们不展开用户的值）：",
            view.dangling_count
        );
        for dangling in &view.dangling {
            println!(
                "  · {}{}",
                dangling.entry.value,
                entry_suffix(dangling.entry.scope, dangling.entry.index)
            );
        }
    }
    println!();

    if view.username_dependencies.is_empty() {
        println!("用户名依赖：没有。");
    } else {
        println!(
            "用户名依赖 {} 条（路径里硬编码了 `\\Users\\<名字>\\`，换账号名就静默失效）：",
            view.username_dependency_count
        );
        for dependency in &view.username_dependencies {
            println!(
                "  · {}{}",
                dependency.entry.value,
                entry_suffix(dependency.entry.scope, dependency.entry.index)
            );
        }
        if view.username_dependencies_at_machine_scope > 0 {
            println!(
                "  ⚠ 其中 {} 条在**机器级** —— 那一类最危险：换账号名之后它会静默失效，\
                 而在这台机器上 `Test-Path` 仍然通过（因为那个目录确实叫这个名字）。",
                view.username_dependencies_at_machine_scope
            );
        }
    }
    println!();

    for line in shadowed_lines(view) {
        println!("{line}");
    }
    println!();

    println!("tuoen **只报告，不自动清理**（决策 24）—— `PATH` 上的东西是你的。");
    println!("要改就用 `tuoen path add <目录>` / `tuoen path remove <目录>`，两者都先出计划。");
    println!("（`show` 本身不写任何东西。）");
}

/// 「遮蔽」那一段的行。
///
/// **抽成纯函数是为了能被测到**：这一段要回答的是"**哪个目录**抢了**哪条命令**"
/// —— 只报条数等于没说。而 `println!` 没法在单测里断言，所以把"行"与"印"分开。
///
/// 三种情形各有各的话：没有 shim 目录、有目录但没人抢、有人抢（逐条点名）。
fn shadowed_lines(view: &PathShowView) -> Vec<String> {
    match &view.shim_dir {
        None => vec!["遮蔽：没有 shim 目录，跳过。".to_owned()],
        Some(dir) if view.shim_commands.is_empty() => vec![format!(
            "遮蔽：我们的 shim 目录（{dir}）里还**没有任何 shim** —— 没得比。\
             先生成几个：`tuoen shim add <工具>`。"
        )],
        Some(dir) if view.shadowed_shims.is_empty() => vec![format!(
            "遮蔽：没有 —— 我们发布的 {} 条命令都是第一个被命中的（shim 目录：{dir}）。",
            view.shim_commands.len()
        )],
        Some(dir) => {
            let mut lines = vec![format!(
                "遮蔽 {} 条（shim 目录：{dir}）—— 下面这些命令**现在跑的不是我们的 shim**：",
                view.shadowed_shim_count
            )];
            for shadowed in &view.shadowed_shims {
                let width = display_width(&shadowed.command).max(4);
                // 一条命令两行：第一行是**哪条命令**，第二行是**谁抢的**（点名目录
                // 与命中的文件）。两行都留着，因为只给目录名答不了"为什么是它"。
                lines.push(format!("  · {}", shadowed.command));
                lines.push(format!(
                    "{:width$}  {} 抢在前面（命中 `{}`）",
                    "",
                    shadowed.by.value,
                    shadowed.file,
                    width = width
                ));
            }
            lines
        }
    }
}

/// `tuoen path add|remove` 的人类输出。
pub fn print_plan_human(view: &PathPlanView) {
    if view.dry_run {
        println!(
            "（演练 —— 什么都没有写{}）",
            if view.will_write {
                "，下面是真跑一遍会做的事"
            } else {
                ""
            }
        );
        println!();
    }

    if let Some(refusal) = view.refusal() {
        // **理由只印一遍。** 完整的解释由编排层的错误路径印在 stderr 上 ——
        // 那一条对"把 stdout 重定向丢掉"的人也自足，与这一族其余的错误一致。
        // 这里只点名稳定错误码（脚本 grep 得到）并说清"一个字节都没写"。
        println!("拒绝[{refusal}]。");
        println!();
        println!("（拒绝路径上一个字节都没有写。理由见 stderr 上的那一行。）");
        return;
    }

    match view.action {
        "add" => println!("把 `{}` 追加到**用户级** `PATH` 的末尾。", view.dir),
        _ => println!("从**用户级** `PATH` 里删掉 `{}` 的全部出现。", view.dir),
    }
    println!("作用域：用户级（`HKCU\\Environment`）—— 机器级要提权，tuoen 不做顺手提权。");
    println!();

    if !view.will_write {
        println!("计划：什么都不做。");
        if let Some(message) = view.noop_message() {
            println!("{message}");
        }
        println!();
        println!("（计划是空操作时**一个字节都不写**，广播也不会发。）");
        return;
    }

    println!(
        "长度：{} → {} 字符（**注册表口径的下界**，不含进程注入；档位 {}；还剩 {} 字符）",
        before_chars(view),
        view.budget_after.registry_chars,
        view.budget_after.level,
        view.budget_after.remaining
    );
    println!();

    for change in &view.changes {
        match change {
            PathChange::Add { value, at } => {
                println!("  + 追加 `{value}`（第 {at} 段之后）");
            }
            PathChange::Remove { value, was_at } => {
                println!("  − 删掉 `{value}`（原来在第 {was_at} 段）");
            }
            PathChange::Retype { from, to } => println!(
                "  ~ 注册表类型 {} → {}（值含 `%` 才用 `expand-sz`，否则保留原类型）",
                reg_type_slug(from),
                reg_type_slug(to)
            ),
            PathChange::DropEmptySegments { count } => {
                println!("  · 顺带清掉 {count} 个空段（`;;`）");
            }
            PathChange::Noop { value, reason } => {
                println!("  · `{value}`：{}", noop_reason_human(*reason, &view.dir));
            }
        }
    }
    println!();

    match &view.applied {
        Some(applied) if applied.wrote => {
            println!(
                "✓ 已写入用户级 `PATH`：{} 字符，类型 {}。",
                applied.chars, applied.reg_type
            );
            println!(
                "  广播 `WM_SETTINGCHANGE`：{} 个顶层窗口应答 —— \
                 它只说明广播发出去了，**不是「所有进程都更新了」**。",
                applied.broadcast_replies
            );
            println!();
            println!(
                "**已经跑着的终端 / IDE 拿不到新环境**（环境块是 `CreateProcess` 时复制的）。"
            );
            println!("要让它生效：新开一个终端。这不是 tuoen 偷懒，是平台限制。");
        }
        Some(_) => {
            println!("没有写（计划与现状一致）。");
        }
        None => {
            println!("以上是计划，**什么都没有写**。去掉 `--dry-run` 才会落盘。");
        }
    }

    if view.shadow_report_worth_printing() {
        println!();
        for line in plan_shadow_lines(view) {
            println!("{line}");
        }
    }
}

/// 「遮蔽」那一段的行（`path add` / `remove` 之后）。
///
/// **抽成纯函数是为了能被测到** —— 与 `shadowed_lines` 同一个理由：
/// `println!` 在单测里断言不了，而这一段的三句话（没得比 / 都没输 / 输了几条）
/// 差一个字就是一句假话。
///
/// **它只报告，绝不自动修**（决策 73）：修遮蔽意味着改动用户 `PATH` 里一条**别的**
/// 目录，那是另一次写入、要有它自己的计划和同意，不能藏在 `add` 的尾巴里顺手做掉。
fn plan_shadow_lines(view: &PathPlanView) -> Vec<String> {
    if view.shim_commands.is_empty() {
        // **不能报"一条都没输"。** 目录是空的（或者还不存在），
        // 没有分母的时候"零条被遮蔽"是一个空洞的结论，说出口就是假话。
        return vec![
            format!(
                "遮蔽：我们的 shim 目录（`{}`）里还**没有任何 shim**，这次没得比。",
                view.shim_dir.as_deref().unwrap_or("?")
            ),
            "  先生成几个：`tuoen shim add node`（它会写进上面那个目录）。".to_owned(),
        ];
    }
    if view.shadowed_shim_count == 0 {
        return vec![format!(
            "遮蔽：我们的 {} 条命令（{}）**全部**是生效顺序里第一个被命中的，没有输给谁。",
            view.shim_commands.len(),
            view.shim_commands.join("、")
        )];
    }

    let mut lines = vec![
        format!(
            "遮蔽：我们发布的命令里有 **{} 条**被别的目录抢在前面（一共 {} 条）。",
            view.shadowed_shim_count,
            view.shim_commands.len()
        ),
        String::new(),
    ];
    for shadow in &view.shadowed_shims {
        lines.push(format!(
            "  ! `{}` → 先命中的是 `{}\\{}`（{}）",
            shadow.command,
            shadow.by.value,
            shadow.file,
            scope_label(shadow.by.scope)
        ));
    }
    lines.push(String::new());
    lines.push(
        "`path add` 只把我们的目录**追加到末尾**，所以它**修不好遮蔽**：\
         名字冲突永远是先命中的赢，而「谁在前」由整条 `PATH` 的顺序决定。"
            .to_owned(),
    );
    lines.push("要修只有两条路：".to_owned());

    let scope_of = |wanted: &str| {
        view.shadowed_shims
            .iter()
            .any(|shadow| shadow.by.scope == wanted)
    };
    if scope_of("user") {
        lines.push(
            "  · 抢在前面的是**用户级**条目 → `tuoen path remove <那条目录>` \
             （一次只删一条，删完再跑 `tuoen path show` 复核）。"
                .to_owned(),
        );
    }
    if scope_of("machine") {
        lines.push(
            "  · 抢在前面的是**机器级**条目 → 要管理员权限：系统属性 → 环境变量，\
             把那条移到最后或删掉。**tuoen 不做顺手提权**，也不会替你改机器级。"
                .to_owned(),
        );
    }
    if scope_of("process-only") {
        lines.push(
            "  · 抢在前面的是**进程注入**的条目（不在注册表里）→ 换一个新终端再看一次；\
             它可能是当前这个 shell 自己的启动器加进去的。"
                .to_owned(),
        );
    }
    lines.push(String::new());
    lines.push("复核：`tuoen path show`（它的遮蔽那一段与这里同一份判据）。".to_owned());
    lines
}

/// 写之前的注册表口径长度（机器级 + 用户级原文）。
///
/// `PathPlan::budget_after` 只给了"写之后"的那两个数，而人类输出里
/// "多少 → 多少"是同一句话的两半，所以这里补出前一半。
fn before_chars(view: &PathPlanView) -> usize {
    view.budget_after
        .registry_chars
        .saturating_sub(char_delta(&view.before_raw, &view.after_raw))
}

/// 原文长度之差 —— 只用于人类输出里的"多少 → 多少"。
fn char_delta(before: &str, after: &str) -> usize {
    after.chars().count().saturating_sub(before.chars().count())
}

/// 作用域的人话。
#[must_use]
pub fn scope_label(scope: &str) -> &'static str {
    match scope {
        "machine" => "机器级",
        "user" => "用户级",
        "process-only" => "进程注入",
        _ => "作用域不明",
    }
}

/// 生效顺序的一行预览。
///
/// **只报前几条**：本机实测 `PATH` 有 48 条，把它们全部铺开会让"顺序"这件事
/// 淹没在噪声里，而顺序才是这一段要说的东西。
fn effective_preview(effective: &[EntryView]) -> String {
    const SHOWN: usize = 6;
    let mut parts: Vec<String> = effective
        .iter()
        .take(SHOWN)
        .map(|entry| format!("[{}]{}", scope_label(entry.scope), entry.value))
        .collect();
    if effective.len() > SHOWN {
        parts.push(format!("……还有 {} 条", effective.len() - SHOWN));
    }
    parts.join(" → ")
}

/// 一条条目的定位后缀：`（机器级 第 12 段）`。
fn entry_suffix(scope: &str, index: usize) -> String {
    format!("（{} 第 {} 段）", scope_label(scope), index)
}

/// 一条条目的完整标签：`（机器级 第 12 段）C:\a;` —— 原文原样，含尾部分号。
fn entry_label(scope: &str, index: usize, raw: &str) -> String {
    format!("{}{}", entry_suffix(scope, index), raw)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tuoen_platform::{EnvScope, NoopReason, PathChange, RegType};

    fn has_cjk(text: &str) -> bool {
        text.chars()
            .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
    }

    #[test]
    fn the_budget_level_slugs_are_the_four_from_the_platform() {
        // 这四个取值是公开契约（决策 35）；改它们要递增 `schemaVersion`。
        for level in [
            tuoen_platform::path::BudgetLevel::Ok,
            tuoen_platform::path::BudgetLevel::Warning,
            tuoen_platform::path::BudgetLevel::Critical,
            tuoen_platform::path::BudgetLevel::Exceeded,
        ] {
            let slug = level.as_str();
            assert!(
                matches!(slug, "ok" | "warning" | "critical" | "exceeded"),
                "档位 slug 只能是这四个，实际 `{slug}`"
            );
        }
    }

    #[test]
    fn a_change_serialises_with_a_stable_kind_slug() {
        // **这是 `--json` 里 `changes[]` 的形状**，也是消费者唯一要分支的东西。
        let add = serde_json::to_string(&PathChange::Add {
            value: r"C:\a".to_owned(),
            at: 3,
        })
        .expect("serialise");
        assert_eq!(add, r#"{"kind":"add","value":"C:\\a","at":3}"#);

        let drop =
            serde_json::to_string(&PathChange::DropEmptySegments { count: 2 }).expect("serialise");
        assert_eq!(drop, r#"{"kind":"drop-empty-segments","count":2}"#);

        let noop = serde_json::to_string(&PathChange::Noop {
            value: r"C:\a".to_owned(),
            reason: NoopReason::AlreadyPresent,
        })
        .expect("serialise");
        assert_eq!(
            noop,
            r#"{"kind":"noop","value":"C:\\a","reason":"already-present"}"#
        );
    }

    #[test]
    fn only_the_protected_shim_dir_is_a_refusal() {
        // “已经在里面”与“本来就不在”都是**成功的空操作**；拒绝只有一种。
        let noop = |reason| PathChange::Noop {
            value: r"C:\a".to_owned(),
            reason,
        };
        assert_eq!(noop_refusal(&noop(NoopReason::AlreadyPresent)), None);
        assert_eq!(noop_refusal(&noop(NoopReason::Absent)), None);
        assert_eq!(
            noop_refusal(&noop(NoopReason::ProtectedShimDir)),
            Some("protected-shim-dir")
        );
        assert_eq!(
            noop_refusal(&PathChange::Add {
                value: r"C:\a".to_owned(),
                at: 0
            }),
            None
        );
    }

    #[test]
    fn registry_type_slugs_are_lowercase_kebab_ascii() {
        assert_eq!(reg_type_slug(&RegType::Sz), "sz");
        assert_eq!(reg_type_slug(&RegType::ExpandSz), "expand-sz");
        for slug in [
            reg_type_slug(&RegType::Sz),
            reg_type_slug(&RegType::ExpandSz),
        ] {
            assert!(
                !slug.is_empty()
                    && slug
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{slug}` 不是小写 kebab ASCII"
            );
        }
    }

    #[test]
    fn scope_slugs_come_from_the_platform_and_have_a_chinese_face() {
        assert_eq!(EnvScope::Machine.as_str(), "machine");
        assert_eq!(EnvScope::User.as_str(), "user");
        assert!(has_cjk(scope_label("machine")), "人话要是中文");
        assert!(has_cjk(scope_label("user")), "人话要是中文");
        // 认不出来的时候也要说人话，而不是把 slug 原样吐出去。
        assert!(has_cjk(scope_label("weird")));
    }

    #[test]
    fn the_noop_explanations_are_chinese_and_name_what_was_not_done() {
        for reason in [
            NoopReason::AlreadyPresent,
            NoopReason::Absent,
            NoopReason::ProtectedShimDir,
        ] {
            let text = noop_reason_human(reason, r"C:\shims");
            assert!(has_cjk(&text), "{reason:?} 的解释要是中文：{text}");
        }
        // 拒绝那一条必须说清"摘掉它等于什么"，以及"你自己手动改"。
        let refusal = noop_reason_human(NoopReason::ProtectedShimDir, r"C:\shims");
        assert!(refusal.contains("shim"), "{refusal}");
        assert!(refusal.contains("不许"), "{refusal}");
    }

    #[test]
    fn a_show_payload_of_an_empty_machine_still_has_every_top_level_key() {
        // 一台什么都没有的机器也要给出**完整的形状** —— 消费者不该为
        // "这台机器干净"写一条特殊的解析路径。
        let analysis = PathAnalysis {
            scopes: Vec::new(),
            effective: Vec::new(),
            budget: PathBudget::of(0, 0),
            process_only: Vec::new(),
            duplicates: Vec::new(),
            dangling: Vec::new(),
            username_dependencies: Vec::new(),
            shadowed_shims: Vec::new(),
            shim_commands: Vec::new(),
        };
        let view = PathShowView::new(&analysis, None);
        let json = serde_json::to_string(&view).expect("serialise");
        for key in [
            "\"name\":\"Path\"",
            "\"budget\":",
            "\"scopes\":[]",
            "\"effective\":[]",
            "\"machineEntries\":0",
            "\"userEntries\":0",
            "\"processOnly\":[]",
            "\"duplicates\":[]",
            "\"dangling\":[]",
            "\"usernameDependencies\":[]",
            "\"shadowedShims\":[]",
            "\"shimDir\":null",
            "\"level\":\"ok\"",
        ] {
            assert!(json.contains(key), "缺少 {key}：{json}");
        }
        assert!(!has_cjk(&json), "成功载荷里不该有中文：{json}");
    }

    #[test]
    fn the_effective_order_is_reported_verbatim_and_is_not_reshuffled() {
        // **`effective` 的顺序就是答案**：谁赢名字冲突由它说了算。
        // 视图层绝不许按 scope 重新分组 —— 那会把"注入项排在机器级前面"
        // 这件本机实测的事实抹掉。
        let entry = |scope, index, value: &str| tuoen_platform::EntryRef {
            scope,
            index,
            raw: value.to_owned(),
            value: value.to_owned(),
        };
        let analysis = PathAnalysis {
            scopes: Vec::new(),
            effective: vec![
                entry(EnvScope::ProcessOnly, 0, r"C:\injected"),
                entry(EnvScope::Machine, 0, r"C:\Windows"),
                entry(EnvScope::User, 0, r"C:\tools"),
            ],
            budget: PathBudget::of(0, 0),
            process_only: vec![r"C:\injected".to_owned()],
            duplicates: Vec::new(),
            dangling: Vec::new(),
            username_dependencies: Vec::new(),
            shadowed_shims: Vec::new(),
            shim_commands: Vec::new(),
        };
        let view = PathShowView::new(&analysis, None);
        let scopes: Vec<&str> = view.effective.iter().map(|entry| entry.scope).collect();
        assert_eq!(
            scopes,
            vec!["process-only", "machine", "user"],
            "生效顺序必须逐条照原样报，不能按作用域重新排"
        );
        let json = serde_json::to_string(&view).expect("serialise");
        assert!(json.contains("\"scope\":\"process-only\""), "{json}");
        assert!(!has_cjk(&json), "{json}");
    }

    #[test]
    fn the_shadowing_section_names_the_command_and_the_directory_that_steals_it() {
        // 票据的原话是"**点名**哪个目录抢了哪条命令"。只报一个条数等于没说 ——
        // 用户拿着"遮蔽 1 条"什么都做不了，拿着"`node` 被 `C:\nvm4w\nodejs` 抢了"
        // 才知道该动谁。三种情形各有各的话，所以三条都钉住。
        let entry = |scope, index, value: &str| tuoen_platform::EntryRef {
            scope,
            index,
            raw: value.to_owned(),
            value: value.to_owned(),
        };
        let analysis = PathAnalysis {
            scopes: Vec::new(),
            effective: Vec::new(),
            budget: PathBudget::of(0, 0),
            process_only: Vec::new(),
            duplicates: Vec::new(),
            dangling: Vec::new(),
            username_dependencies: Vec::new(),
            shim_commands: vec!["node".to_owned()],
            shadowed_shims: vec![tuoen_platform::ShadowedShim {
                command: "node".to_owned(),
                by: entry(EnvScope::Machine, 18, r"C:\nvm4w\nodejs"),
                file: "node.exe".to_owned(),
                shim_dir: r"C:\Users\x\AppData\Local\tuoen\shims".to_owned(),
            }],
        };

        // ① 有人抢：点名命令 + 点名目录 + 点中命中的文件。
        let view = PathShowView::new(&analysis, Some(r"C:\shims"));
        let text = shadowed_lines(&view).join("\n");
        assert!(text.contains("node"), "{text}");
        assert!(text.contains(r"C:\nvm4w\nodejs"), "要点名是谁抢的：{text}");
        assert!(text.contains("node.exe"), "要说清命中的是哪个文件：{text}");
        assert!(text.contains("遮蔽 1 条"), "{text}");

        // ② 有 shim 目录但没人抢 —— 这句话本身也是结论。
        let quiet = PathAnalysis {
            shadowed_shims: Vec::new(),
            ..analysis.clone()
        };
        let quiet_view = PathShowView::new(&quiet, Some(r"C:\shims"));
        let text = shadowed_lines(&quiet_view).join("\n");
        assert!(text.contains("没有"), "{text}");
        assert!(
            text.contains(r"C:\shims"),
            "要说清是按哪个 shim 目录判的：{text}"
        );

        // ③ 连 shim 目录都不知道（算不出家目录）—— 不许假装"没有遮蔽"。
        let unknown = PathShowView::new(&quiet, None);
        let text = shadowed_lines(&unknown).join("\n");
        assert!(text.contains("没有 shim 目录"), "{text}");
        assert!(
            !text.contains("没有 ——"),
            "「算不出来」与「没人抢」是两回事，不许混成一句：{text}"
        );
    }

    /// 空 shim 目录**不能**说成"都没输"。
    ///
    /// 这是实测抓出来的：`shadowedShimCount=0` 有两个完全不同的成因 ——
    /// "我们的命令都赢了"和"我们一条命令都还没发布"。原来的判据只看
    /// `shadowed_shims.is_empty()`，于是空目录也会被报成前者（一句假话）。
    /// 分母是 `shim_commands`，所以它必须在。
    #[test]
    fn an_empty_shim_directory_is_not_reported_as_winning() {
        let analysis = PathAnalysis {
            scopes: Vec::new(),
            effective: Vec::new(),
            budget: PathBudget::of(0, 0),
            process_only: Vec::new(),
            duplicates: Vec::new(),
            dangling: Vec::new(),
            username_dependencies: Vec::new(),
            shadowed_shims: Vec::new(),
            shim_commands: Vec::new(),
        };

        let view = PathShowView::new(&analysis, Some(r"C:\shims"));
        let text = shadowed_lines(&view).join("\n");
        assert!(text.contains("没有任何 shim"), "空目录要说没得比：{text}");
        assert!(
            !text.contains("都是第一个被命中的"),
            "空目录**不许**说成「都没输」：{text}"
        );

        // 有 4 条命令、一条都没被抢 —— 这才是"都没输"。
        let with_commands = PathAnalysis {
            shim_commands: vec![
                "corepack".to_owned(),
                "node".to_owned(),
                "npm".to_owned(),
                "npx".to_owned(),
            ],
            ..analysis
        };
        let view = PathShowView::new(&with_commands, Some(r"C:\shims"));
        let text = shadowed_lines(&view).join("\n");
        assert!(text.contains("4 条命令"), "要报出分母：{text}");
        assert!(text.contains("都是第一个被命中的"), "{text}");
    }

    /// `path add` / `remove` 之后的遮蔽报告（决策 73）。
    ///
    /// 三句话各有各的触发条件，而**说错一句就是假话**，所以三条都钉住：
    /// 空 shim 目录 → "没得比"；有权重洁 → "都没输"；被抢 → 逐条点名 + 可执行的下一步。
    #[test]
    fn the_plan_shadow_report_distinguishes_no_match_from_not_losing() {
        let plan = PathPlan {
            scope: EnvScope::User,
            before_raw: r"C:\a".to_owned(),
            after_raw: r"C:\a;C:\shims".to_owned(),
            before_type: RegType::Sz,
            after_type: RegType::Sz,
            changes: Vec::new(),
            changes_value: true,
            budget_after: PathBudget::of(10, 10),
        };

        // ① 一条 shim 都没有 —— 不许说"都没输"。
        let empty = PathPlanView::new("add", r"C:\shims", &plan, true, None).with_shadowing(
            &PathAnalysis {
                scopes: Vec::new(),
                effective: Vec::new(),
                budget: PathBudget::of(0, 0),
                process_only: Vec::new(),
                duplicates: Vec::new(),
                dangling: Vec::new(),
                username_dependencies: Vec::new(),
                shadowed_shims: Vec::new(),
                shim_commands: Vec::new(),
            },
            Some(Path::new(r"C:\shims")),
        );
        let text = plan_shadow_lines(&empty).join("\n");
        assert!(text.contains("没有任何 shim"), "{text}");
        assert!(
            !text.contains("都没有") && !text.contains("全部**是"),
            "{text}"
        );
        assert_eq!(empty.shim_dir.as_deref(), Some(r"C:\shims"));

        // ② 有 shim、没被抢 —— 这一句才是结论，而且要报出分母。
        let clean = PathPlanView::new("add", r"C:\shims", &plan, true, None).with_shadowing(
            &PathAnalysis {
                scopes: Vec::new(),
                effective: Vec::new(),
                budget: PathBudget::of(0, 0),
                process_only: Vec::new(),
                duplicates: Vec::new(),
                dangling: Vec::new(),
                username_dependencies: Vec::new(),
                shadowed_shims: Vec::new(),
                shim_commands: vec!["node".to_owned(), "npm".to_owned()],
            },
            Some(Path::new(r"C:\shims")),
        );
        let text = plan_shadow_lines(&clean).join("\n");
        assert!(text.contains("2 条命令"), "{text}");
        assert!(text.contains("没有输给谁"), "{text}");

        // ③ 被机器级抢了 —— 点名 + 说出"只能手动改机器级"。
        let shadowed = PathPlanView::new("add", r"C:\shims", &plan, true, None).with_shadowing(
            &PathAnalysis {
                scopes: Vec::new(),
                effective: Vec::new(),
                budget: PathBudget::of(0, 0),
                process_only: Vec::new(),
                duplicates: Vec::new(),
                dangling: Vec::new(),
                username_dependencies: Vec::new(),
                shim_commands: vec!["node".to_owned(), "npm".to_owned()],
                shadowed_shims: vec![tuoen_platform::ShadowedShim {
                    command: "node".to_owned(),
                    by: tuoen_platform::EntryRef {
                        scope: EnvScope::Machine,
                        index: 18,
                        raw: r"C:\nvm4w\nodejs".to_owned(),
                        value: r"C:\nvm4w\nodejs".to_owned(),
                    },
                    file: "node.exe".to_owned(),
                    shim_dir: r"C:\shims".to_owned(),
                }],
            },
            Some(Path::new(r"C:\shims")),
        );
        let text = plan_shadow_lines(&shadowed).join("\n");
        assert!(text.contains("**1 条**"), "要报条数：{text}");
        assert!(text.contains("（一共 2 条）"), "要报分母：{text}");
        assert!(
            text.contains(r"C:\nvm4w\nodejs\node.exe"),
            "要点名是谁：{text}"
        );
        assert!(text.contains("机器级"), "要说清是哪个作用域：{text}");
        assert!(
            !text.contains("用户级**条目"),
            "没被用户级抢就不该给用户级的办法：{text}"
        );
        assert_eq!(shadowed.shadowed_shim_count, 1);
    }

    /// 什么情况下这一整段值得印。
    ///
    /// 两种：真被抢了（必须说）；或者这次加的就是我们自己的 shim 目录
    /// （用户这次的目的正是"让 tuoen 的命令能用"，那"都没输"是他等着听的好消息）。
    /// 给不相干的目录加 PATH 时不该跟一段遮蔽报告 —— 那是噪声。
    #[test]
    fn the_shadow_report_is_printed_only_when_it_says_something() {
        let analysis = PathAnalysis {
            scopes: Vec::new(),
            effective: Vec::new(),
            budget: PathBudget::of(0, 0),
            process_only: Vec::new(),
            duplicates: Vec::new(),
            dangling: Vec::new(),
            username_dependencies: Vec::new(),
            shadowed_shims: Vec::new(),
            shim_commands: vec!["node".to_owned()],
        };
        let plan = PathPlan {
            scope: EnvScope::User,
            before_raw: String::new(),
            after_raw: r"C:\x".to_owned(),
            before_type: RegType::Sz,
            after_type: RegType::Sz,
            changes: Vec::new(),
            changes_value: true,
            budget_after: PathBudget::of(1, 1),
        };

        // 不相干的目录 + 没人抢 → 不印。
        let other = PathPlanView::new("add", r"C:\x", &plan, true, None)
            .with_shadowing(&analysis, Some(Path::new(r"C:\shims")));
        assert!(!other.shadow_report_worth_printing());

        // 加的正是我们的 shim 目录 → 印（哪怕结论是"都没输"）。
        let ours = PathPlanView::new("add", r"C:\shims", &plan, true, None)
            .with_shadowing(&analysis, Some(Path::new(r"C:\shims")));
        assert!(ours.shadow_report_worth_printing());

        // 大小写与结尾反斜杠不算差别（用户敲的就是这两种形态）。
        let casing = PathPlanView::new("add", r"c:\SHIMS\", &plan, true, None)
            .with_shadowing(&analysis, Some(Path::new(r"C:\shims")));
        assert!(
            casing.shadow_report_worth_printing(),
            "`c:\\SHIMS\\` 与 `C:\\shims` 是同一个目录"
        );

        // 有人抢 → 印（不管加的是哪个目录）。
        let stolen = PathAnalysis {
            shadowed_shims: vec![tuoen_platform::ShadowedShim {
                command: "node".to_owned(),
                by: tuoen_platform::EntryRef {
                    scope: EnvScope::Machine,
                    index: 0,
                    raw: r"C:\nvm4w\nodejs".to_owned(),
                    value: r"C:\nvm4w\nodejs".to_owned(),
                },
                file: "node.exe".to_owned(),
                shim_dir: r"C:\shims".to_owned(),
            }],
            ..analysis
        };
        let stolen_view = PathPlanView::new("add", r"C:\x", &plan, true, None)
            .with_shadowing(&stolen, Some(Path::new(r"C:\shims")));
        assert!(stolen_view.shadow_report_worth_printing());
    }
}
