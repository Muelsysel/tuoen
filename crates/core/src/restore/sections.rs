//! 四个 section 的判据 —— **一张比较表，不是四条流程**。
//!
//! 每个 `evaluate_*` 收下同一形状的两侧（目标快照 / 本机现场），答三个问题：
//!
//! 1. 这一节要做什么？（`actions`，全 ASCII 的稳定 slug）
//! 2. 真会写下去几条？（`counts.effective`，决策 158 的第二个口径）
//! 3. 哪些事我们做不到、为什么做不到？（`manual_actions`，决策 152 的公开契约）
//!
//! # 四节的口径都不一样，这不是不一致，是事实
//!
//! | section | 目标 | 默认动作 | 有没有 `effective` |
//! |---|---|---|---|
//! | `tools` | 补上本机缺的工具 | `install` | 有（`install` 的条数） |
//! | `path` | 把 `PATH` 重建到快照的样子 | `add`/`remove`/`move`/`case-only`（**不含 `fix`**） | 有（真落地的条数，决策 158） |
//! | `env` | 补上缺的持久环境变量 | `set-user` / `set-machine` | 有（`set-user`/`set-machine`） |
//! | `wsl` | **只报告** | 什么都不做 | **恒空**（`note = "report-only"`） |
//!
//! # 为什么 restore 从不卸载、从不覆盖、从不清理
//!
//! 三条都是"看起来更彻底"的路，也都是这个工具最容易变成灾难的地方：
//!
//! * 本机多出来的工具**只计数**（`extra`），不卸载 —— 那可能是用户自己装的别的东西；
//! * 本机已有的环境变量**只计数**（`present`），不改值 —— 覆盖别人的配置不是"还原"；
//! * 本机 `PATH` 自己的病（重复 / 失效 / 空条目）默认**只报告**（`fix` 不进默认选择）——
//!   决策 50/67 已经定过"绝不自动清理幽灵条目"，而 151 把它落到了这一票。
//!
//! # status 是"最需要用户注意的那一件事"（决策 161）
//!
//! 顺序固定、先命中先算：`unsupported`（且没有任何会写的动作）→ `needs-network` →
//! `requires-elevation` → `would-change` → `no-change`。
//! `needs_network` / `requires_elevation` 两个 bool 与 `status` **正交**：
//! 一节里既有要装的工具又有要提权的机器级写入时，`status` 印 `needs-network`，
//! 两个 bool 都为真。

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use tuoen_platform::{DirEntryFacts, EnvScope, FileFacts, FileSystem};

use crate::capture::{EnvFile, PathBudgetRow, PathFile, SkippedFile, ToolRow, ToolsFile, WslFile};
use crate::pathdiff::{self, DiffClass, PathDiffOptions, PathDiffRow, Selection};

use super::manual::{
    ManualAction, ManualActionCode, REMEDIATION_INSTALL_MANUALLY,
    REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION, REMEDIATION_RECONFIGURE_MANUALLY,
    REMEDIATION_RUN_AS_ADMINISTRATOR, REMEDIATION_USE_THE_MANAGER, ascii_token, dedupe_manual,
};
use super::plan::{PlannedAction, RestoreOptions, SectionCounts, SectionPlan};

// ─────────────────────────────────────────────────────────────────────────────
// section 的身份
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/` 里的一个 section，也是 `--only <section>` 的取值表。
///
/// **取值表只有这一处定义**（决策 159）：CLI 的 `--only` 与 `sections_present()`
/// 都走 [`SectionId::ALL`] / [`SectionId::parse`]，不另抄一份字符串。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SectionId {
    /// `tools.toml`。
    Tools,
    /// `path.toml`。
    Path,
    /// `env.toml`。
    Env,
    /// `wsl.toml`。
    Wsl,
}

impl SectionId {
    /// 全部四个，**顺序即计划里 `sections` 的顺序**（固定顺序，两次调用逐条相同）。
    pub const ALL: [Self; 4] = [Self::Tools, Self::Path, Self::Env, Self::Wsl];

    /// 稳定小写 slug（`--json` 与 `tuoen.d/` 的文件名同源）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Path => "path",
            Self::Env => "env",
            Self::Wsl => "wsl",
        }
    }

    /// 从 slug 解析。**精确匹配**（大小写在这里是数据）。
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.as_str() == name)
    }
}

impl std::fmt::Display for SectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 一节的状态。**"最需要用户注意的那一件事"**（决策 161），不是"这一节有没有差异"。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SectionStatus {
    /// 这一节什么都不用做（或者只剩报告）。
    NoChange,
    /// 有会写的动作，但不需要提权也不需要网络。
    WouldChange,
    /// 有机器级写入 —— **tuoen 不自己提权**（决策 136），要用户以管理员身份做。
    RequiresElevation,
    /// 有要下载的步骤（`install`）。**计划永远不需要网络**（决策 157），
    /// 需要网络的是"照这个计划做"这件事。
    NeedsNetwork,
    /// 只剩做不到的事：有 `unsupported` 动作、且没有任何会写的动作。
    Unsupported,
    /// 这一节压根没参与：没被 `--only` 选中，或者快照里没有它（决策 159/161）。
    Skipped,
}

impl SectionStatus {
    /// 六个状态，**顺序即决策 161 的阶梯**（CLI 生成帮助时的稳定顺序）。
    pub const ALL: [Self; 6] = [
        Self::NoChange,
        Self::WouldChange,
        Self::RequiresElevation,
        Self::NeedsNetwork,
        Self::Unsupported,
        Self::Skipped,
    ];

    /// 稳定小写 slug（进 `--json`，**不本地化**）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoChange => "no-change",
            Self::WouldChange => "would-change",
            Self::RequiresElevation => "requires-elevation",
            Self::NeedsNetwork => "needs-network",
            Self::Unsupported => "unsupported",
            Self::Skipped => "skipped",
        }
    }

    /// 这个状态算不算"有变更"（[`super::RestorePlan::has_changes`] 的判据）。
    ///
    /// **`needs-network` 算**（决策 161）：要装三个工具的计划的 `status` 就是它，
    /// 不算的话 CLI 会走"空计划 → 打印无变更"那条路（决策 155），
    /// 而那句话是**假的**。
    #[must_use]
    pub const fn is_change(self) -> bool {
        matches!(
            self,
            Self::WouldChange | Self::RequiresElevation | Self::NeedsNetwork
        )
    }
}

/// 快照里没有这一节（决策 159）。**不是错误**，是一句可见的说明。
pub const NOTE_SECTION_NOT_IN_SNAPSHOT: &str = "section-not-in-snapshot";
/// 这一节没被 `--only` 选中（决策 161）。与上一个**不能混用**：用户看到的话完全不同。
pub const NOTE_NOT_SELECTED: &str = "not-selected";
/// `PATH` 里有本机自身的健康问题、而默认不应用它们（决策 151）。
pub const NOTE_FIX_NOT_SELECTED: &str = "fix-not-selected";
/// 这一节要提权（`status` 已经是 `requires-elevation`，这句是给人类输出的说明）。
pub const NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION: &str = "machine-scope-requires-elevation";
/// 这一节要下载才能做。
pub const NOTE_NEEDS_NETWORK: &str = "needs-network";
/// 这一节只报告差异，**不会写任何东西**（`wsl`：我们不自动导入 vhdx）。
pub const NOTE_REPORT_ONLY: &str = "report-only";

// ─────────────────────────────────────────────────────────────────────────────
// 判据用的小工具
// ─────────────────────────────────────────────────────────────────────────────

/// 一节里"会写"的动作有哪些 —— 决策 161 的阶梯要的就是这四个 bool。
#[derive(Debug, Default, Clone, Copy)]
struct Writes {
    /// 有 `install` 动作（要下载）。
    installs: bool,
    /// 有任何**真会写下去**的动作。
    writes: bool,
    /// 其中有机器级的（要提权）。
    machine_writes: bool,
    /// 有 `unsupported` 动作（做不到的事）。
    unsupported: bool,
}

/// 决策 161 的阶梯：先命中先算。
fn status_for(writes: Writes) -> SectionStatus {
    if writes.unsupported && !writes.writes {
        SectionStatus::Unsupported
    } else if writes.installs {
        SectionStatus::NeedsNetwork
    } else if writes.machine_writes {
        SectionStatus::RequiresElevation
    } else if writes.writes {
        SectionStatus::WouldChange
    } else {
        SectionStatus::NoChange
    }
}

/// 组装一节。
fn assemble(
    id: SectionId,
    counts: SectionCounts,
    actions: Vec<PlannedAction>,
    writes: Writes,
    note: Option<&'static str>,
) -> (SectionPlan, Vec<ManualAction>) {
    let plan = SectionPlan {
        id,
        status: status_for(writes),
        counts,
        actions,
        needs_network: writes.installs,
        requires_elevation: writes.machine_writes,
        note: note.map(str::to_owned),
    };
    (plan, Vec::new())
}

/// 组装一节 + 它带出来的手动待办。
fn assemble_with(
    id: SectionId,
    counts: SectionCounts,
    actions: Vec<PlannedAction>,
    writes: Writes,
    note: Option<&'static str>,
    manual: &mut Vec<ManualAction>,
) -> (SectionPlan, Vec<ManualAction>) {
    dedupe_manual(manual);
    let (plan, _) = assemble(id, counts, actions, writes, note);
    (plan, std::mem::take(manual))
}

/// 一个"什么都不知道"的文件系统：任何路径都不存在，任何目录都是空的。
///
/// `plan` 是纯函数（不碰磁盘），而 `pathdiff::diff` 的签名要一个 `FileSystem` ——
/// 它**只**用这个参数判"拼写疑似"（决策 133：一条失效条目的父目录里有没有
/// 编辑距离 ≤ 2 的邻居）。那件事只有在**本机的实时 diff** 里才有意义：
/// 计划面对的是两份快照，问磁盘等于把"另一台机器的路径在不在"混进来。
///
/// 于是这里如实回答"我不知道"：`suggestion` 恒为 `None`。
/// [`crate::pathdiff::SideRow`] 里的 `exists` 三态**不受影响** —— 那是快照里的数据。
#[derive(Debug, Default, Clone, Copy)]
struct NoDisk;

impl FileSystem for NoDisk {
    fn inspect(&self, path: &Path) -> FileFacts {
        FileFacts::missing(path)
    }

    fn list_dir(&self, _: &Path) -> Vec<DirEntryFacts> {
        Vec::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// tools
// ─────────────────────────────────────────────────────────────────────────────

/// **已知"因再分发许可被拒"的工具名 → `detail` slug。**
///
/// 名字不在表里就是"这个版本还不支持"，**不是**"许可被拒" —— 把"不知道"写成
/// "许可问题"正是票据禁止的那种无用信息（决策 160）。表里当前只有一项：
/// Oracle JDK 8/11/17 的 BCL 不允许再分发（`docs/DESIGN.md` §3）。
const LICENCE_BLOCKED: &[(&str, &str)] =
    &[("oracle-jdk", "oracle-jdk-redistribution-not-permitted")];

/// 本机已经有这条工具了吗（**同名同版本**；目标行没版本时同名即可）。
fn installed_locally(row: &ToolRow, local: &[ToolRow]) -> bool {
    local.iter().any(|other| {
        other.name.eq_ignore_ascii_case(&row.name)
            && match &row.version {
                None => true,
                Some(version) => other
                    .version
                    .as_deref()
                    .is_some_and(|mine| same_version(version, mine)),
            }
    })
}

/// 同一个版本号吗。**只做规范化，不做前缀匹配** —— "同名同版本"是精确的一件事，
/// 而 `24` 匹配 `24.19.0` 是 `pathdiff`/manifest 的"约束匹配"，是另一件事。
fn same_version(wanted: &str, mine: &str) -> bool {
    fn norm(text: &str) -> &str {
        text.trim().trim_start_matches(['v', 'V'])
    }
    norm(wanted).eq_ignore_ascii_case(norm(mine))
}

/// `tools` section。
///
/// 判据顺序固定四条（决策 160，先命中先算），**先看本机有没有**：
/// ⓪ 本机已有同名同版本 → 只计数（`installed`），**不进 `actions`**；
/// ① 缺失 + 有 `manager` → `third-party-managed`（**不碰它的符号链接/环境变量/settings.txt**）；
/// ② 缺失 + `reproducible` → `install`（`needs_network`）；
/// ③ 缺失 + 不可复现 + 名字命中 [`LICENCE_BLOCKED`] → `unsupported` + `licence-blocked`；
/// ④ 其余缺失 + 不可复现 → `unsupported` + `unsupported`。
///
/// `manual_actions` 里 `third-party-manager` **对本机已经装好的行也出**（按管理器去重）：
/// 它说的不是"缺一个工具"，而是"这个工具归它管、tuoen 永远不接管" —— 而这一条
/// 与本机有没有它无关。`unsupported` / `licence-blocked` 则**只在真的需要动作时**才出
/// （否则"还原到本机"会吐出 12 条噪声，而票据要的是接近空的计划）。
///
/// **本机已有的行只计数**，而"本机已有"的判据是**同名同版本** ——
/// 目标行没版本时同名即可（那种行的版本我们问不出来，按"有"算比按"缺"算安全：
/// 按缺算会去装一个可能已经在的工具）。
pub(crate) fn tools(
    target: &ToolsFile,
    local: Option<&ToolsFile>,
) -> (SectionPlan, Vec<ManualAction>) {
    let local_rows: &[ToolRow] = local.map_or(&[], |file| file.tool.as_slice());
    let mut counts = SectionCounts::with_keys(&[
        "installed",
        "missing",
        "third-party",
        "unsupported",
        "extra",
    ]);
    let mut actions: Vec<PlannedAction> = Vec::new();
    let mut manual: Vec<ManualAction> = Vec::new();
    let mut managers: Vec<String> = Vec::new();
    let mut unsupported: Vec<String> = Vec::new();
    let mut blocked: Vec<(String, &'static str)> = Vec::new();
    let mut writes = Writes::default();

    for row in &target.tool {
        let name = ascii_token(&row.name);
        // **管理器这件事与本机装没装无关**：这条待办说的是"这个工具归它管，tuoen 不接管"
        // （决策 154），本机已经有它并没有让这个问题消失。`counts.rows` 仍然如实按
        // "本机有没有"分类（已装 → `installed`），两件事互不覆盖。
        if let Some(manager) = row.manager.as_deref() {
            let manager = ascii_token(manager);
            if !managers.iter().any(|known| known == &manager) {
                managers.push(manager);
            }
        }
        if installed_locally(row, local_rows) {
            counts.bump_row("installed");
            continue;
        }
        // ── ① 第三方版本管理器管的：动作是"我们不管"，待办是"你用它自己" ──
        if let Some(manager) = row.manager.as_deref() {
            let manager = ascii_token(manager);
            counts.bump_row("third-party");
            actions.push(PlannedAction::new(
                format!("tools:{name}"),
                "third-party-managed",
                &name,
                tools_detail(row, Some(&manager)),
            ));
            continue;
        }
        // ── ② 可复现 → 装它。这是这一票唯一会下载的动作 ──
        if row.reproducible {
            counts.bump_row("missing");
            counts.bump_effective("install");
            writes.installs = true;
            writes.writes = true;
            actions.push(PlannedAction::new(
                format!("tools:{name}"),
                "install",
                &name,
                tools_detail(row, None),
            ));
            continue;
        }
        // ── ③④ 不可复现：许可被拒的给具体 slug，其余一律"这个版本不支持" ──
        let slug = LICENCE_BLOCKED
            .iter()
            .find(|(tool, _)| tool.eq_ignore_ascii_case(&row.name))
            .map(|(_, slug)| *slug);
        counts.bump_row("unsupported");
        writes.unsupported = true;
        actions.push(PlannedAction::new(
            format!("tools:{name}"),
            "unsupported",
            &name,
            tools_detail(row, None),
        ));
        match slug {
            Some(slug) => blocked.push((name, slug)),
            None => unsupported.push(name),
        }
    }

    // 本机多出来的工具：**只计数，从不卸载**。
    for row in local_rows {
        if !target
            .tool
            .iter()
            .any(|other| other.name.eq_ignore_ascii_case(&row.name))
        {
            counts.bump_row("extra");
        }
    }

    // 待办：① 每个管理器一条；③④ 每个工具一条。
    for manager in &managers {
        manual.push(ManualAction::new(
            ManualActionCode::ThirdPartyManager,
            manager,
            manager,
            REMEDIATION_USE_THE_MANAGER,
        ));
    }
    for (name, slug) in &blocked {
        manual.push(ManualAction::new(
            ManualActionCode::LicenceBlocked,
            name,
            slug,
            REMEDIATION_INSTALL_MANUALLY,
        ));
    }
    for name in &unsupported {
        manual.push(ManualAction::new(
            ManualActionCode::Unsupported,
            name,
            "not-reproducible",
            REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION,
        ));
    }

    // 工具装进我们自己的存储（`%LOCALAPPDATA%\tuoen`），**不需要提权** ——
    // 所以这一节永远不产出 `requires-elevation`（决策 160 的"仅当真的需要"）。
    assemble_with(SectionId::Tools, counts, actions, writes, None, &mut manual)
}

/// `tools` 的 `detail`：`recipe=node version=24.19.0 manager=nvm4w`（决策 152 的形状）。
///
/// **只用 `name` / `version` / `manager` 三个字段**：`path` 可能是
/// `<无 InstallLocation，卸载键 {GUID}>` 这种给「人」看的占位符，`evidence` 是中文散文，
/// 两者都没有进计划的入口。
fn tools_detail(row: &ToolRow, manager: Option<&str>) -> String {
    let mut parts = vec![format!("recipe={}", ascii_token(&row.name))];
    if let Some(version) = &row.version {
        parts.push(format!("version={}", ascii_token(version)));
    }
    if let Some(manager) = manager {
        parts.push(format!("manager={manager}"));
    }
    parts.join(" ")
}

// ─────────────────────────────────────────────────────────────────────────────
// path
// ─────────────────────────────────────────────────────────────────────────────

/// `path` 的 `counts` 键，**逐字**照决策 158 写：`caseOnly` 是数据键，
/// 不是 `DiffClass` 的序列化（那是 `case-only`）。
const PATH_KEYS: [&str; 6] = ["keep", "add", "remove", "move", "fix", "caseOnly"];

/// 空 `path.toml`（本机侧没有这一节时用）。预算只是表头，重建会自己算。
fn empty_path_file(captured_at: &str) -> PathFile {
    PathFile::new(
        captured_at,
        PathBudgetRow {
            raw_user_chars: 0,
            raw_machine_chars: 0,
            effective_chars: 0,
            cliff: tuoen_platform::PATH_CLIFF_CMD,
            remaining: tuoen_platform::PATH_CLIFF_CMD,
            level: "ok".to_owned(),
        },
    )
}

/// `path` section：复用 §1.17 的 `diff` + `rebuild` + `Selection`。
///
/// **默认选择 = `add`/`remove`/`move`/`case-only`，不含 `fix`**（决策 151）：
/// 本机 `PATH` 自己的病（重复 / 失效 / 空条目）不是"与快照的差异"，
/// 默认应用它们等于"你没要求却顺手清理了幽灵条目"。`fix` 仍然**报告条数**
/// 并给 `note = "fix-not-selected"`；`--with-fix` 才选中它。
///
/// `actions` 是**被选中的那些类**的行（`keep` 永远不进），`counts.rows` 是全部六类的条数，
/// `counts.effective` 是**真会写下去的条数** —— 两个口径都要出（决策 158）。
pub(crate) fn path(
    target: &PathFile,
    local: Option<&PathFile>,
    opts: &RestoreOptions,
) -> (SectionPlan, Vec<ManualAction>) {
    let empty = empty_path_file(&target.captured_at);
    let local = local.unwrap_or(&empty);

    let diff = pathdiff::diff(
        local,
        target,
        &NoDisk,
        &PathDiffOptions {
            current_username: opts.current_username.as_deref(),
        },
    );

    let mut classes = vec![
        DiffClass::Add,
        DiffClass::Remove,
        DiffClass::Move,
        DiffClass::CaseOnly,
    ];
    if opts.with_fix {
        classes.push(DiffClass::Fix);
    }
    let selection = Selection::new(classes, Vec::new());
    let rebuilt = pathdiff::rebuild(local, target, &diff, &selection);

    let mut counts = SectionCounts::with_keys(&PATH_KEYS);
    counts.set_row("keep", diff.counts.keep as u64);
    counts.set_row("add", diff.counts.add as u64);
    counts.set_row("remove", diff.counts.remove as u64);
    counts.set_row("move", diff.counts.move_ as u64);
    counts.set_row("fix", diff.counts.fix as u64);
    counts.set_row("caseOnly", diff.counts.case_only as u64);

    // `effective`：**真会写的条数**。判据是重建产出的 `applied`，但有一条例外 ——
    // "插入被输出不变量跳过"那一条在 `applied` 里记成 `fix`（决策 158 的真机实测），
    // 而它**什么都没落**：它是一条与已有条目重复、被跳过的插入。
    // 判据是 `from_index == None`（真正的 fix 一定改的是某一条已有的行，有来源下标）。
    let mut writes = Writes::default();
    let mut landed: BTreeMap<String, usize> = BTreeMap::new();
    for scope in &rebuilt.scopes {
        for applied in &scope.applied {
            let Some(key) = effective_key(applied) else {
                continue;
            };
            counts.bump_effective(key);
            writes.writes = true;
            landed.insert(applied.id.clone(), applied.to_index);
            if scope.scope == EnvScope::Machine {
                writes.machine_writes = true;
            }
        }
    }

    let actions: Vec<PlannedAction> = diff
        .rows
        .iter()
        .filter(|row| row.class != DiffClass::Keep && selection.selects_class(row.class))
        .map(|row| {
            let detail = landed
                .get(&row.id)
                .map_or_else(|| "no-op".to_owned(), |at| format!("toIndex={at}"));
            PlannedAction::new(
                row.id.clone(),
                row.class.as_str(),
                &path_subject(row),
                detail,
            )
        })
        .collect();

    let mut manual: Vec<ManualAction> = Vec::new();
    if writes.machine_writes {
        manual.push(ManualAction::new(
            ManualActionCode::RequiresElevation,
            tuoen_platform::PATH_NAME,
            &format!("scope=machine var={}", tuoen_platform::PATH_NAME),
            REMEDIATION_RUN_AS_ADMINISTRATOR,
        ));
    }

    // `note` 只有一个字段，而两句话都成立时先印哪一句是要选的：
    // `fix-not-selected` 优先 —— 决策 151 **要求**报告"本机自己有 24 条病、我没动"，
    // 而提权这件事已经由 `status` + `requires_elevation` 表达了。
    let note = if diff.counts.fix > 0 && !opts.with_fix {
        Some(NOTE_FIX_NOT_SELECTED)
    } else if writes.machine_writes {
        Some(NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION)
    } else {
        None
    };

    assemble_with(SectionId::Path, counts, actions, writes, note, &mut manual)
}

/// 这条 `applied` 真的落进新列表了吗？落了的话算哪一类。
///
/// `None` 的两种情形：`fix` + `from_index == None`（被跳过的插入，**什么都没落**）、
/// 以及重建根本不产出的 `keep` / `case-only`。
fn effective_key(applied: &crate::pathdiff::AppliedRow) -> Option<&'static str> {
    match applied.class {
        DiffClass::Add => Some("add"),
        DiffClass::Remove => Some("remove"),
        DiffClass::Move => Some("move"),
        DiffClass::Fix if applied.from_index.is_some() => Some("fix"),
        DiffClass::Fix | DiffClass::Keep | DiffClass::CaseOnly => None,
    }
}

/// 一条 `PATH` 差异行的 `subject`：**本机侧那条的值**（本机没有就取目标侧）。
///
/// 值是用户认得出来的东西（`C:\nvm4w\nodejs`），而下标（`user:12`）在报告里
/// 不如它直观 —— 下标仍然留在 `id` 里（`--pick` 用的就是它）。
fn path_subject(row: &PathDiffRow) -> String {
    row.local
        .as_ref()
        .or(row.target.as_ref())
        .map_or_else(|| ascii_token(&row.id), |side| ascii_token(&side.value))
}

// ─────────────────────────────────────────────────────────────────────────────
// env
// ─────────────────────────────────────────────────────────────────────────────

/// `credential-named` / `credential` —— 两种都表示"这里有一个我们**没有捕获**的凭据"。
///
/// 票据点名的触发条件是 `credential-named`（名字像凭据）；`credential`
/// （值像凭据）是同一个结论的另一条路。**只认 `credential-named` 会让后一种
/// 静默消失** —— 而"静默跳过是 bug"是本仓库写在 `capture` 里的第一条不变量。
fn is_credential_kind(kind: &str) -> bool {
    matches!(kind, "credential-named" | "credential")
}

/// 目标快照里被跳过的凭据，按（名字忽略大小写，作用域，kind）排序 —— 稳定的输出顺序。
fn credential_skips(skipped: Option<&SkippedFile>) -> Vec<&crate::capture::SkipEntry> {
    let mut entries: Vec<&crate::capture::SkipEntry> = skipped
        .map(|file| {
            file.skipped
                .iter()
                .filter(|e| is_credential_kind(&e.kind))
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by(|a, b| {
        (a.name.to_lowercase(), a.scope.clone(), a.kind.clone()).cmp(&(
            b.name.to_lowercase(),
            b.scope.clone(),
            b.kind.clone(),
        ))
    });
    entries
}

/// `env` section：补上本机缺的持久环境变量。
///
/// * 用户级 → `set-user`；机器级 → `set-machine` + `requires_elevation`
///   + 手动待办 `requires-elevation`（**tuoen 绝不自己弹 UAC**，决策 136）；
/// * **本机已有的（同名同作用域）只计数**，永不覆盖 —— 值不同也不覆盖，
///   详见 `docs/acceptance` 与本模块的"未覆盖项"；
/// * `skipped.toml` 里因凭据被跳过的名字 → `skipped-secret`，**绝不进任何"要写"的清单**
///   （它不进 `effective`），并产出 `credential-reconfigure` 待办，`subject` 是
///   **标识**、`detail` 是 kind 的 slug，**绝不含任何材料**（决策 153）。
pub(crate) fn env(
    target: &EnvFile,
    local: Option<&EnvFile>,
    skipped: Option<&SkippedFile>,
) -> (SectionPlan, Vec<ManualAction>) {
    let local_vars: &[crate::capture::EnvVarRow] = local.map_or(&[], |file| file.var.as_slice());
    let mut counts = SectionCounts::with_keys(&[
        "present",
        "missing-user",
        "missing-machine",
        "secret-skipped",
    ]);
    let mut actions: Vec<PlannedAction> = Vec::new();
    let mut manual: Vec<ManualAction> = Vec::new();
    let mut writes = Writes::default();

    for var in &target.var {
        let name = ascii_token(&var.name);
        if local_vars
            .iter()
            .any(|mine| mine.scope == var.scope && mine.name.eq_ignore_ascii_case(&var.name))
        {
            counts.bump_row("present");
            continue;
        }
        match var.scope {
            EnvScope::User => {
                counts.bump_row("missing-user");
                counts.bump_effective("set-user");
                writes.writes = true;
                actions.push(PlannedAction::new(
                    format!("env:user:{name}"),
                    "set-user",
                    &name,
                    "scope=user",
                ));
            }
            EnvScope::Machine => {
                counts.bump_row("missing-machine");
                counts.bump_effective("set-machine");
                writes.writes = true;
                writes.machine_writes = true;
                actions.push(PlannedAction::new(
                    format!("env:machine:{name}"),
                    "set-machine",
                    &name,
                    "scope=machine",
                ));
                manual.push(ManualAction::new(
                    ManualActionCode::RequiresElevation,
                    &name,
                    &format!("scope=machine var={name}"),
                    REMEDIATION_RUN_AS_ADMINISTRATOR,
                ));
            }
            // `env.toml` 只写 user / machine（进程环境不是可搬运的状态）。
            // 一份外来快照里真出现 process-only 行时，我们既不计数也不写它 ——
            // 现有 `counts` 键里没有它的位置，详见报告的"未覆盖项"。
            EnvScope::ProcessOnly => {}
        }
    }

    for entry in credential_skips(skipped) {
        let name = ascii_token(&entry.name);
        // 只有 `env` 那一段的跳过项对应"一个环境变量"，别的 section（未来的 configs）
        // 仍然要产出待办，只是没有环境变量可以指。
        if entry.section == "env" {
            counts.bump_row("secret-skipped");
            actions.push(PlannedAction::new(
                format!("env:{name}"),
                "skipped-secret",
                &name,
                format!("scope={}", entry.scope.as_deref().unwrap_or("unknown")),
            ));
        }
        manual.push(ManualAction::new(
            ManualActionCode::CredentialReconfigure,
            &format!("env:{name}"),
            &entry.kind,
            REMEDIATION_RECONFIGURE_MANUALLY,
        ));
    }

    let note = if writes.machine_writes {
        Some(NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION)
    } else {
        None
    };
    assemble_with(SectionId::Env, counts, actions, writes, note, &mut manual)
}

// ─────────────────────────────────────────────────────────────────────────────
// wsl
// ─────────────────────────────────────────────────────────────────────────────

/// 同一个发行版吗（WSL 的名字不区分大小写）。
fn same_distro(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// 同一个 `BasePath` 吗（Windows 路径：去尾部反斜杠 + 忽略大小写）。
fn same_path(a: &str, b: &str) -> bool {
    fn norm(text: &str) -> &str {
        text.trim_end_matches(['\\', '/'])
    }
    norm(a).eq_ignore_ascii_case(norm(b))
}

/// `wsl` section：**只报告差异，什么都不写**。
///
/// 于是 `counts.effective` 恒空、`note = "report-only"`、`status` 恒 `no-change`
/// —— 哪怕差异一大堆。这不是谦虚：**`status = would-change` 会让预览对用户说
/// "会改"而实际上一个字节都不会写**，而那正是这一票从头到尾在防的那类错话
/// （决策 147/148 的同一条）。
///
/// **不自动导入 vhdx**：镜像可能几十 GB，而且 `wsl --import` 会把发行版装到一个
/// 我们猜出来的位置；报告里给的是目标快照里那个**实际** `BasePath`。
pub(crate) fn wsl(target: &WslFile, local: Option<&WslFile>) -> (SectionPlan, Vec<ManualAction>) {
    let local_dists: &[crate::capture::WslRow] =
        local.map_or(&[], |file| file.distribution.as_slice());
    let mut counts = SectionCounts::with_keys(&["same", "missing", "extra", "path-differs"]);
    let mut actions: Vec<PlannedAction> = Vec::new();

    for distro in &target.distribution {
        let name = ascii_token(&distro.name);
        match local_dists
            .iter()
            .find(|mine| same_distro(&mine.name, &distro.name))
        {
            None => {
                counts.bump_row("missing");
                actions.push(PlannedAction::new(
                    format!("wsl:{name}"),
                    "missing-distro",
                    &name,
                    format!("distro={name}"),
                ));
            }
            Some(mine) if !same_path(&mine.base_path, &distro.base_path) => {
                counts.bump_row("path-differs");
                actions.push(PlannedAction::new(
                    format!("wsl:{name}"),
                    "path-differs",
                    &name,
                    format!(
                        "distro={name} local={} target={}",
                        ascii_token(&mine.base_path),
                        ascii_token(&distro.base_path)
                    ),
                ));
            }
            Some(_) => counts.bump_row("same"),
        }
    }

    for distro in local_dists {
        if !target
            .distribution
            .iter()
            .any(|other| same_distro(&other.name, &distro.name))
        {
            counts.bump_row("extra");
            let name = ascii_token(&distro.name);
            actions.push(PlannedAction::new(
                format!("wsl:{name}"),
                "extra-distro",
                &name,
                format!("distro={name}"),
            ));
        }
    }

    // effective 恒空：这一节没有任何"真会写"的东西。
    assemble(
        SectionId::Wsl,
        counts,
        actions,
        Writes::default(),
        Some(NOTE_REPORT_ONLY),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Existence;

    use crate::restore::test_support as fixture;

    fn counts_of(plan: &SectionPlan) -> Vec<(String, u64)> {
        plan.counts
            .rows
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect()
    }

    #[test]
    fn section_slugs_round_trip_and_are_kebab() {
        for id in SectionId::ALL {
            assert_eq!(SectionId::parse(id.as_str()), Some(id));
            assert!(!id.as_str().contains('_'), "slug 不许有下划线");
        }
        assert_eq!(SectionId::parse("envs"), None);
        let slugs: Vec<&str> = SectionStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            slugs,
            vec![
                "no-change",
                "would-change",
                "requires-elevation",
                "needs-network",
                "unsupported",
                "skipped",
            ]
        );
    }

    #[test]
    fn only_needs_network_requires_elevation_and_would_change_count_as_changes() {
        assert!(SectionStatus::WouldChange.is_change());
        assert!(SectionStatus::RequiresElevation.is_change());
        assert!(
            SectionStatus::NeedsNetwork.is_change(),
            "要装工具的计划不算有变更 → CLI 会印'无变更'（决策 161）"
        );
        assert!(!SectionStatus::NoChange.is_change());
        assert!(!SectionStatus::Unsupported.is_change());
        assert!(!SectionStatus::Skipped.is_change());
    }

    // ── tools ────────────────────────────────────────────────────────────

    #[test]
    fn a_missing_reproducible_tool_is_an_install_that_needs_network() {
        let target = fixture::tools(&[("node", Some("24.19.0"), None, true)]);
        let (plan, manual) = tools(&target, None);
        assert_eq!(plan.status, SectionStatus::NeedsNetwork);
        assert!(plan.needs_network && !plan.requires_elevation);
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].kind, "install");
        assert_eq!(plan.actions[0].id, "tools:node");
        assert_eq!(plan.actions[0].subject, "node");
        assert_eq!(plan.actions[0].detail, "recipe=node version=24.19.0");
        assert_eq!(plan.counts.rows["missing"], 1);
        assert_eq!(plan.counts.effective["install"], 1);
        assert!(
            manual.is_empty(),
            "工具装进我们自己的存储，不需要提权 —— 这条待办不该凭空出现"
        );
        assert_eq!(plan.note, None);
    }

    #[test]
    fn an_already_installed_tool_is_only_counted() {
        let target = fixture::tools(&[("node", Some("24.19.0"), None, true)]);
        let local = fixture::tools(&[("node", Some("24.19.0"), None, true)]);
        let (plan, _) = tools(&target, Some(&local));
        assert_eq!(plan.status, SectionStatus::NoChange);
        assert!(plan.actions.is_empty(), "已经有它了，不进 actions");
        assert_eq!(plan.counts.rows["installed"], 1);
        assert!(plan.counts.effective.is_empty());
    }

    #[test]
    fn a_different_version_is_not_the_same_tool() {
        let target = fixture::tools(&[("node", Some("24.19.0"), None, true)]);
        let local = fixture::tools(&[("node", Some("22.0.0"), None, true)]);
        let (plan, _) = tools(&target, Some(&local));
        assert_eq!(plan.actions.len(), 1, "版本不同 → 还得装");
        assert_eq!(plan.counts.rows["missing"], 1);
        assert_eq!(plan.counts.rows["extra"], 0, "同名就不算 extra");
    }

    #[test]
    fn a_target_row_without_a_version_matches_by_name_alone() {
        let target = fixture::tools(&[("python", None, None, true)]);
        let local = fixture::tools(&[("python", Some("3.12.1"), None, true)]);
        let (plan, _) = tools(&target, Some(&local));
        assert_eq!(
            plan.counts.rows["installed"], 1,
            "版本问不出来时按'有'算 —— 按缺算会去装一个可能已经在的工具"
        );
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn a_third_party_managed_tool_is_not_touched_and_is_deduped_by_manager() {
        let target = fixture::tools(&[
            ("node", Some("24.19.0"), Some("nvm4w"), false),
            ("python", Some("3.13.0"), Some("uv"), false),
            ("python3", Some("3.13.0"), Some("uv"), false),
        ]);
        let (plan, manual) = tools(&target, None);
        assert_eq!(plan.counts.rows["third-party"], 3);
        assert_eq!(plan.counts.rows["missing"], 0, "别人的工具不是我们的缺口");
        assert!(!plan.needs_network, "不接管 = 不下载");
        assert_eq!(plan.status, SectionStatus::NoChange, "我们什么都不写");
        assert!(plan.counts.effective.is_empty());
        assert!(plan.actions.iter().all(|a| a.kind == "third-party-managed"));
        let subjects: Vec<&str> = manual.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(
            subjects,
            ["nvm4w", "uv"],
            "按管理器去重，不是每个工具行一条"
        );
        assert!(
            manual
                .iter()
                .all(|m| m.code == ManualActionCode::ThirdPartyManager)
        );
        assert_eq!(manual[0].detail, "nvm4w");
        assert_eq!(manual[0].remediation, REMEDIATION_USE_THE_MANAGER);
    }

    #[test]
    fn a_licence_blocked_tool_gets_a_specific_reason_slug() {
        let target = fixture::tools(&[("oracle-jdk", Some("8u491"), None, false)]);
        let (plan, manual) = tools(&target, None);
        assert_eq!(plan.actions[0].kind, "unsupported", "不装它");
        assert_eq!(plan.counts.rows["unsupported"], 1);
        assert!(!plan.needs_network);
        assert_eq!(plan.status, SectionStatus::Unsupported);
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].code, ManualActionCode::LicenceBlocked);
        assert_eq!(manual[0].subject, "oracle-jdk");
        assert_eq!(
            manual[0].detail, "oracle-jdk-redistribution-not-permitted",
            "detail 必须是具体原因 slug，不是 'licence'、不是空串"
        );
        assert_eq!(manual[0].remediation, REMEDIATION_INSTALL_MANUALLY);
    }

    #[test]
    fn an_unknown_unreproducible_tool_is_not_reported_as_a_licence_problem() {
        let target = fixture::tools(&[("weird-tool", Some("1.0"), None, false)]);
        let (_, manual) = tools(&target, None);
        assert_eq!(manual[0].code, ManualActionCode::Unsupported);
        assert_eq!(
            manual[0].detail, "not-reproducible",
            "名字不在 LICENCE_BLOCKED 里 = '这个版本不支持'，不是'许可被拒'"
        );
        assert_eq!(manual[0].subject, "weird-tool");
        assert_eq!(
            manual[0].remediation,
            REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION
        );
    }

    #[test]
    fn the_four_step_order_is_manager_then_install_then_licence_then_unsupported() {
        // ① 有 manager 就是别人的工具，哪怕它同时又不可复现、又命中许可表。
        let managed = fixture::tools(&[("oracle-jdk", Some("8"), Some("sdkman"), false)]);
        let (plan, manual) = tools(&managed, None);
        assert_eq!(plan.actions[0].kind, "third-party-managed");
        assert_eq!(manual[0].code, ManualActionCode::ThirdPartyManager);

        // ② 可复现优先于许可表：我们打算自己装，就不该报"许可被拒"。
        let reproducible = fixture::tools(&[("oracle-jdk", Some("8"), None, true)]);
        let (plan, manual) = tools(&reproducible, None);
        assert_eq!(plan.actions[0].kind, "install");
        assert!(manual.is_empty(), "②命中时不该有 licence-blocked");

        // ③④ 不可复现 + 命中表 / 不命中表。
        let blocked = fixture::tools(&[("oracle-jdk", Some("8"), None, false)]);
        let (_, manual) = tools(&blocked, None);
        assert_eq!(manual[0].code, ManualActionCode::LicenceBlocked);
        let other = fixture::tools(&[("ruby", Some("3"), None, false)]);
        let (_, manual) = tools(&other, None);
        assert_eq!(manual[0].code, ManualActionCode::Unsupported);
    }

    #[test]
    fn a_manager_todo_is_reported_even_when_the_tool_is_already_here() {
        // 真机形状：`node` 本机有（nvm4w 装的）、`uv` 管着 python —— 两行都"已满足"，
        // 但"tuoen 不接管这两个管理器"这件事仍然要说（决策 154）。
        let target = fixture::tools(&[
            ("node", Some("24.19.0"), Some("nvm4w"), false),
            ("python", Some("3.13.0"), Some("uv"), false),
        ]);
        let local = fixture::tools(&[
            ("node", Some("24.19.0"), Some("nvm4w"), false),
            ("python", Some("3.13.0"), Some("uv"), false),
        ]);
        let (plan, manual) = tools(&target, Some(&local));
        assert_eq!(plan.counts.rows["installed"], 2);
        assert_eq!(
            plan.counts.rows["third-party"], 0,
            "本机已经有它了，不是缺口"
        );
        assert!(plan.actions.is_empty(), "已满足的行不进 actions");
        assert_eq!(plan.status, SectionStatus::NoChange);
        let subjects: Vec<&str> = manual.iter().map(|m| m.subject.as_str()).collect();
        assert_eq!(subjects, ["nvm4w", "uv"], "两条待办照出，且按管理器去重");
    }

    #[test]
    fn extra_local_tools_are_counted_and_never_uninstalled() {
        let target = fixture::tools(&[("node", Some("24.19.0"), None, true)]);
        let local = fixture::tools(&[
            ("node", Some("24.19.0"), None, true),
            ("rust", Some("1.88"), None, true),
            ("go", Some("1.24"), None, true),
        ]);
        let (plan, _) = tools(&target, Some(&local));
        assert_eq!(plan.counts.rows["extra"], 2);
        assert!(
            plan.actions
                .iter()
                .all(|a| a.kind != "remove" && a.kind != "uninstall"),
            "restore 从不卸载"
        );
        assert!(plan.actions.is_empty());
    }

    #[test]
    fn chinese_evidence_and_placeholder_paths_never_reach_the_plan() {
        // 真机上这两样都存在，而且都是"看起来完全合理"的污染源。
        let mut row = fixture::tool("cargo", Some("1.88.0"), None, true);
        row.evidence = "PATH 第 1 条（process-only）里有 cargo.exe".to_owned();
        row.path = "<无 InstallLocation，卸载键 {GUID}>".to_owned();
        let target = fixture::tools_file(vec![row]);
        let (plan, manual) = tools(&target, None);
        assert!(
            plan.actions[0].detail.is_ascii(),
            "{}",
            plan.actions[0].detail
        );
        assert!(!plan.actions[0].detail.contains("InstallLocation"));
        assert!(!plan.actions[0].subject.contains('（'));
        assert!(manual.is_empty());
    }

    // ── path ─────────────────────────────────────────────────────────────

    #[test]
    fn an_identical_path_produces_no_actions_at_all() {
        let file = fixture::path(&[
            (EnvScope::Machine, r"C:\Windows", Existence::Yes),
            (EnvScope::User, r"C:\nvm4w\nodejs", Existence::Yes),
        ]);
        let (plan, manual) = path(&file, Some(&file), &RestoreOptions::all());
        assert_eq!(plan.status, SectionStatus::NoChange);
        assert!(plan.actions.is_empty());
        assert!(manual.is_empty());
        assert_eq!(plan.counts.rows["keep"], 2);
        assert!(plan.counts.effective.is_empty());
        assert_eq!(plan.note, None);
    }

    #[test]
    fn path_defaults_exclude_fix_but_still_report_the_count() {
        // `""` 是一段**空条目**（`;;` 那种）；两行 `C:\a` 让第二行成为 duplicate。
        let rows = [
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
        ];
        let target = fixture::path(&rows);
        let local = fixture::path(&rows);
        let (plan, _) = path(&target, Some(&local), &RestoreOptions::all());
        assert_eq!(plan.counts.rows["fix"], 2, "重复一条 + 空条目一条");
        assert_eq!(
            plan.actions.iter().filter(|a| a.kind == "fix").count(),
            0,
            "默认不选中 fix → 不许有 fix 动作"
        );
        assert_eq!(plan.note.as_deref(), Some(NOTE_FIX_NOT_SELECTED));
        assert_eq!(plan.status, SectionStatus::NoChange);
    }

    #[test]
    fn path_with_fix_selects_the_health_problems_too() {
        let rows = [
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, r"C:\a", Existence::Yes),
            (EnvScope::User, "", Existence::Unknown),
        ];
        let target = fixture::path(&rows);
        let local = fixture::path(&rows);
        let opts = RestoreOptions {
            with_fix: true,
            ..RestoreOptions::all()
        };
        let (plan, _) = path(&target, Some(&local), &opts);
        let kinds: Vec<&str> = plan.actions.iter().map(|a| a.kind.as_str()).collect();
        assert!(kinds.iter().all(|k| *k == "fix"), "{kinds:?}");
        assert_eq!(plan.actions.len(), plan.counts.rows["fix"] as usize);
        assert!(plan.counts.effective["fix"] >= 1);
        assert_eq!(plan.status, SectionStatus::WouldChange);
        assert_eq!(plan.note, None);
    }

    #[test]
    fn two_identical_adds_only_one_lands() {
        // 决策 158 的真机实测形状：目标侧重复两条 → `add 2` 但只有 1 条落地。
        let target = fixture::path(&[
            (EnvScope::User, r"C:\Development", Existence::Yes),
            (EnvScope::User, r"C:\Development", Existence::Yes),
        ]);
        let local = fixture::path(&[(EnvScope::User, r"C:\Windows", Existence::Yes)]);
        let (plan, _) = path(&target, Some(&local), &RestoreOptions::all());
        assert_eq!(plan.counts.rows["add"], 2, "分类是**行**的口径");
        assert_eq!(plan.counts.effective["add"], 1, "写下去是**列表**的口径");
        let adds: Vec<&PlannedAction> = plan.actions.iter().filter(|a| a.kind == "add").collect();
        assert_eq!(
            adds.len(),
            2,
            "两行都在 actions 里（它说的是'这一类有两行'）"
        );
        let landed = adds
            .iter()
            .filter(|a| a.detail.starts_with("toIndex="))
            .count();
        assert_eq!(
            landed,
            1,
            "只有一条真的插进去了，另一条记 no-op：{:?}",
            adds.iter().map(|a| &a.detail).collect::<Vec<_>>()
        );
        assert_eq!(plan.status, SectionStatus::WouldChange);
    }

    #[test]
    fn case_only_is_selected_but_does_nothing() {
        let target = fixture::path(&[(EnvScope::User, r"C:\Tools", Existence::Yes)]);
        let local = fixture::path(&[(EnvScope::User, r"c:\tools", Existence::Yes)]);
        let (plan, _) = path(&target, Some(&local), &RestoreOptions::all());
        assert_eq!(plan.counts.rows["caseOnly"], 1);
        assert_eq!(plan.actions.len(), 1);
        assert_eq!(plan.actions[0].kind, "case-only");
        assert_eq!(
            plan.actions[0].detail, "no-op",
            "决策 128/144：改大小写零收益、纯风险 —— 计划要说清它不做任何改写"
        );
        assert!(plan.counts.effective.is_empty());
        assert_eq!(plan.status, SectionStatus::NoChange);
    }

    #[test]
    fn a_machine_scope_change_requires_elevation_and_says_so() {
        let target = fixture::path(&[
            (EnvScope::Machine, r"C:\Windows", Existence::Yes),
            (EnvScope::Machine, r"C:\Dev\tools", Existence::Yes),
        ]);
        let local = fixture::path(&[(EnvScope::Machine, r"C:\Windows", Existence::Yes)]);
        let (plan, manual) = path(&target, Some(&local), &RestoreOptions::all());
        assert!(plan.requires_elevation);
        assert_eq!(plan.status, SectionStatus::RequiresElevation);
        assert_eq!(
            plan.note.as_deref(),
            Some(NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION)
        );
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].code, ManualActionCode::RequiresElevation);
        assert_eq!(manual[0].subject, tuoen_platform::PATH_NAME);
        // 值是**注册表里那个值名**（`Path`，判据只有 `tuoen_platform::PATH_NAME` 一处定义）——
        // 决策 152 的例写成 `var=PATH`，那是大家口语里的写法。
        assert_eq!(
            manual[0].detail,
            format!("scope=machine var={}", tuoen_platform::PATH_NAME)
        );
        assert_eq!(manual[0].remediation, REMEDIATION_RUN_AS_ADMINISTRATOR);
    }

    #[test]
    fn a_user_scope_change_only_would_change() {
        let target = fixture::path(&[(EnvScope::User, r"C:\Dev\tools", Existence::Yes)]);
        let local = fixture::path(&[(EnvScope::User, r"C:\Windows", Existence::Yes)]);
        let (plan, manual) = path(&target, Some(&local), &RestoreOptions::all());
        assert_eq!(plan.status, SectionStatus::WouldChange);
        assert!(!plan.requires_elevation);
        assert!(manual.is_empty());
    }

    #[test]
    fn a_local_side_that_is_missing_behaves_like_an_empty_path() {
        let target = fixture::path(&[(EnvScope::User, r"C:\Dev\tools", Existence::Yes)]);
        let (plan, _) = path(&target, None, &RestoreOptions::all());
        assert_eq!(plan.counts.rows["add"], 1);
        assert_eq!(plan.status, SectionStatus::WouldChange);
    }

    // ── env ──────────────────────────────────────────────────────────────

    #[test]
    fn a_missing_user_var_is_set_and_a_missing_machine_var_needs_elevation() {
        let target = fixture::env(&[
            (EnvScope::User, "JAVA_HOME", r"C:\Java"),
            (EnvScope::Machine, "NVM_HOME", r"C:\nvm4w"),
        ]);
        let (plan, manual) = env(&target, None, None);
        assert_eq!(plan.counts.rows["missing-user"], 1);
        assert_eq!(plan.counts.rows["missing-machine"], 1);
        assert_eq!(plan.counts.effective["set-user"], 1);
        assert_eq!(plan.counts.effective["set-machine"], 1);
        assert_eq!(plan.status, SectionStatus::RequiresElevation);
        assert!(plan.requires_elevation);
        assert_eq!(plan.actions[0].detail, "scope=user");
        assert_eq!(plan.actions[1].detail, "scope=machine");
        assert_eq!(manual.len(), 1, "只有机器级那条需要提权");
        assert_eq!(manual[0].subject, "NVM_HOME");
        assert_eq!(manual[0].detail, "scope=machine var=NVM_HOME");
    }

    #[test]
    fn an_existing_variable_is_only_counted_and_never_overwritten() {
        let target = fixture::env(&[(EnvScope::User, "JAVA_HOME", r"C:\Java")]);
        let local = fixture::env(&[(EnvScope::User, "JAVA_HOME", r"C:\OtherJava")]);
        let (plan, _) = env(&target, Some(&local), None);
        assert_eq!(plan.counts.rows["present"], 1);
        assert!(plan.actions.is_empty(), "值不同也不覆盖别人已经设好的东西");
        assert_eq!(plan.status, SectionStatus::NoChange);
    }

    #[test]
    fn the_scope_matters_more_than_the_name() {
        let target = fixture::env(&[(EnvScope::Machine, "NVM_HOME", r"C:\nvm4w")]);
        let local = fixture::env(&[(EnvScope::User, "NVM_HOME", r"C:\nvm4w")]);
        let (plan, _) = env(&target, Some(&local), None);
        assert_eq!(
            plan.counts.rows["missing-machine"], 1,
            "同名但作用域不同是**两件事**（本机 NVM_HOME 真的同时在两个作用域里）"
        );
        assert_eq!(plan.counts.rows["present"], 0);
    }

    #[test]
    fn a_credential_named_skip_is_skipped_secret_and_never_a_write() {
        let target = fixture::env(&[(EnvScope::User, "JAVA_HOME", r"C:\Java")]);
        let skipped = fixture::skipped(&[("env", "ARK_API_KEY", Some("user"), "credential-named")]);
        let (plan, manual) = env(&target, None, Some(&skipped));
        assert_eq!(plan.counts.rows["secret-skipped"], 1);
        let secret = plan
            .actions
            .iter()
            .find(|a| a.kind == "skipped-secret")
            .expect("要有 skipped-secret 动作");
        assert_eq!(secret.id, "env:ARK_API_KEY");
        assert_eq!(secret.subject, "ARK_API_KEY");
        assert_eq!(secret.detail, "scope=user");
        assert!(
            !plan.counts.effective.contains_key(&secret.kind),
            "绝不出现在任何'要写'的清单里"
        );
        assert_eq!(
            plan.counts.effective.keys().collect::<Vec<_>>(),
            vec!["set-user"]
        );
        assert_eq!(manual.len(), 1);
        assert_eq!(manual[0].code, ManualActionCode::CredentialReconfigure);
        assert_eq!(manual[0].subject, "env:ARK_API_KEY");
        assert_eq!(manual[0].detail, "credential-named");
        assert_eq!(manual[0].remediation, REMEDIATION_RECONFIGURE_MANUALLY);
        // 契约：任何字段都不许含材料（这里连值都没有，断言的是"没有可含的东西"）。
        assert!(!manual[0].subject.contains("glpat-"));
    }

    #[test]
    fn a_value_that_looks_like_a_credential_is_reported_too() {
        // `credential-named` 是名字像凭据；`credential` 是值像凭据。
        // 只认前者会让后者静默消失 —— 而"静默跳过是 bug"。
        let skipped = fixture::skipped(&[("env", "SOME_TOKEN", None, "credential")]);
        let (plan, manual) = env(&fixture::env(&[]), None, Some(&skipped));
        assert_eq!(plan.counts.rows["secret-skipped"], 1);
        assert_eq!(manual[0].detail, "credential");
        assert_eq!(manual[0].subject, "env:SOME_TOKEN");
        assert_eq!(
            plan.actions[0].detail, "scope=unknown",
            "跳过的记录没有作用域时如实写 unknown，不猜"
        );
    }

    #[test]
    fn a_credential_skip_does_not_appear_twice_for_two_scopes() {
        let skipped = fixture::skipped(&[
            ("env", "ARK_API_KEY", Some("user"), "credential-named"),
            ("env", "ARK_API_KEY", Some("machine"), "credential-named"),
        ]);
        let (plan, manual) = env(&fixture::env(&[]), None, Some(&skipped));
        assert_eq!(plan.counts.rows["secret-skipped"], 2, "两行都数");
        assert_eq!(manual.len(), 1, "但'重配这个凭据'是一件事");
    }

    #[test]
    fn a_non_credential_skip_is_not_reported_as_a_credential() {
        let skipped = fixture::skipped(&[("env", "SOMETHING", Some("user"), "unreadable")]);
        let (plan, manual) = env(&fixture::env(&[]), None, Some(&skipped));
        assert!(plan.counts.rows["secret-skipped"] == 0);
        assert!(plan.actions.is_empty());
        assert!(manual.is_empty());
    }

    // ── wsl ──────────────────────────────────────────────────────────────

    #[test]
    fn wsl_reports_differences_and_writes_nothing() {
        let target = fixture::wsl(&[
            ("Ubuntu-22.04", r"C:\Users\a\AppData\Local\WSL\Ubuntu-22.04"),
            ("Arch-Linux-current", r"C:\linux\Arch-Linux-current"),
        ]);
        let local = fixture::wsl(&[("Ubuntu-22.04", r"D:\wsl\Ubuntu-22.04")]);
        let (plan, manual) = wsl(&target, Some(&local));
        assert_eq!(
            plan.status,
            SectionStatus::NoChange,
            "只报告 → 预览不许说'会改'"
        );
        assert!(plan.counts.effective.is_empty(), "effective 恒空");
        assert_eq!(plan.note.as_deref(), Some(NOTE_REPORT_ONLY));
        assert!(!plan.needs_network && !plan.requires_elevation);
        assert!(manual.is_empty(), "不自动导入 vhdx");
        assert_eq!(plan.counts.rows["path-differs"], 1);
        assert_eq!(plan.counts.rows["missing"], 1);
        assert_eq!(plan.counts.rows["same"], 0);
        let kinds: Vec<&str> = plan.actions.iter().map(|a| a.kind.as_str()).collect();
        assert_eq!(kinds, ["path-differs", "missing-distro"]);
    }

    #[test]
    fn wsl_extra_distros_are_reported_but_never_removed() {
        let target = fixture::wsl(&[("Ubuntu-22.04", r"C:\wsl\Ubuntu")]);
        let local = fixture::wsl(&[
            ("Ubuntu-22.04", r"C:\wsl\Ubuntu"),
            ("Debian", r"C:\wsl\Debian"),
        ]);
        let (plan, _) = wsl(&target, Some(&local));
        assert_eq!(plan.counts.rows["extra"], 1);
        assert_eq!(plan.counts.rows["same"], 1);
        assert_eq!(plan.actions[0].kind, "extra-distro");
        assert_eq!(plan.status, SectionStatus::NoChange);
    }

    #[test]
    fn wsl_path_comparison_ignores_a_trailing_separator_and_case() {
        let target = fixture::wsl(&[("Ubuntu", r"C:\WSL\ubuntu\")]);
        let local = fixture::wsl(&[("ubuntu", r"c:\wsl\ubuntu")]);
        let (plan, _) = wsl(&target, Some(&local));
        assert_eq!(plan.counts.rows["same"], 1, "尾部反斜杠与大小写不是差异");
        assert!(plan.actions.is_empty());
    }

    // ── 组装 ─────────────────────────────────────────────────────────────

    #[test]
    fn the_status_ladder_matches_decision_161() {
        // ② 只剩做不到的事 → unsupported
        assert_eq!(
            status_for(Writes {
                unsupported: true,
                ..Writes::default()
            }),
            SectionStatus::Unsupported
        );
        // 但只要有会写的动作，unsupported 就不算这一节的主状态
        assert_eq!(
            status_for(Writes {
                unsupported: true,
                installs: true,
                writes: true,
                machine_writes: false,
            }),
            SectionStatus::NeedsNetwork
        );
        assert_eq!(
            status_for(Writes {
                unsupported: true,
                writes: true,
                machine_writes: true,
                installs: false,
            }),
            SectionStatus::RequiresElevation
        );
        // ③ 在 ④ 前面：既有 install 又有机器级写入 → needs-network
        assert_eq!(
            status_for(Writes {
                installs: true,
                writes: true,
                machine_writes: true,
                unsupported: false,
            }),
            SectionStatus::NeedsNetwork
        );
        assert_eq!(
            status_for(Writes {
                writes: true,
                ..Writes::default()
            }),
            SectionStatus::WouldChange
        );
        assert_eq!(status_for(Writes::default()), SectionStatus::NoChange);
    }

    #[test]
    fn counts_rows_always_carry_every_key_of_that_section() {
        let (plan, _) = tools(&fixture::tools(&[]), None);
        assert_eq!(
            counts_of(&plan),
            vec![
                ("extra".to_owned(), 0),
                ("installed".to_owned(), 0),
                ("missing".to_owned(), 0),
                ("third-party".to_owned(), 0),
                ("unsupported".to_owned(), 0),
            ],
            "形状稳定：GUI / 脚本可以无条件取键"
        );
        assert!(
            plan.counts.effective.is_empty(),
            "没写东西时 effective 是空的"
        );
    }
}
