//! 计划的形状与那个**纯函数**入口。
//!
//! [`plan`] 是这一票的全部：`(目标快照, 本机现场, 选项) -> RestorePlan`。
//! 它不碰网络、不碰注册表、不起进程 —— 所以 `--dry-run` 与真做共用**同一份**计划，
//! 而"预览与执行一致"是比出来的，不是承诺出来的（决策 139 的同一条）。
//!
//! # 为什么所有类型都 `derive(Serialize)`
//!
//! `--json` 的形状由 **core 拥有**（§1.17 的先例）：字段 camelCase、枚举 kebab-case、
//! `Option` 一律 `skip_serializing_if`。CLI 只负责包信封，不负责翻译字段名 ——
//! 抄一份字段名会在某次改名之后静默少一个键，而少一个键的 `--json` **看起来完全正常**。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::bundle::RestoreBundle;
use super::manual::ManualAction;
use super::sections::{
    self, NOTE_NOT_SELECTED, NOTE_SECTION_NOT_IN_SNAPSHOT, SectionId, SectionStatus,
};

/// 这次还原要做什么。
///
/// `sections` **空 = 全部**（与 [`crate::capture::CaptureOptions`] 同一条约定：
/// "空"是"我没限制"，不是"什么都别做" —— 后者会让一个忘了传参的调用安静地什么都不干）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreOptions {
    /// 只做这些 section（空 = 四个都做）。
    #[serde(default)]
    pub sections: Vec<SectionId>,
    /// 把本机 `PATH` **自己的**健康问题（重复 / 失效 / 空条目 / 用户名）一起应用。
    ///
    /// 默认**否**（决策 151）：那些不是"与快照的差异"，而是本机自己的病，
    /// 顺手清理它们与本仓库"绝不自动清理幽灵条目"冲突。
    #[serde(default)]
    pub with_fix: bool,
    /// 当前用户名。`None` 就没有用户名重写（宁可少做一步，也不猜）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_username: Option<String>,
    /// **我们自己的根**（`%LOCALAPPDATA%\tuoen\globals`）—— `globals` 那一节装进哪里。
    ///
    /// 它是**传进来的**，不是在这里读进程环境算的：`plan` 必须是纯函数（决策 150），
    /// 而"根在哪"是 CLI 的事（决策 187 的接缝）。`None` = 算不出来 —— 那一节的包
    /// 会被记成 `unsupported` + `reason=globals-root-unknown`，**绝不**静默跳过。
    #[serde(skip)]
    pub globals_base: Option<std::path::PathBuf>,
    /// npm 缓存探针的结果（`<tool>:<name>@<version>` → 能不能离线解决）。
    ///
    /// 空表 = 什么都没探过 ⇒ 一律算"要网络"（**不许**把"没探过"当成"缓存里有"）。
    /// 同样 `#[serde(skip)]`：它是**量出来的事实**，不是用户给的选项。
    #[serde(skip)]
    pub globals_cache: BTreeMap<String, bool>,
}

impl RestoreOptions {
    /// 什么都不限制（五个 section、不带 `--with-fix`、不给用户名、不知道我们的根）。
    #[must_use]
    pub fn all() -> Self {
        Self::default()
    }

    /// 实际要做的 section，**去重并保持 [`SectionId::ALL`] 的固定顺序**。
    #[must_use]
    pub fn effective_sections(&self) -> Vec<SectionId> {
        if self.sections.is_empty() {
            return SectionId::ALL.to_vec();
        }
        SectionId::ALL
            .into_iter()
            .filter(|id| self.sections.contains(id))
            .collect()
    }
}

/// 计划里的一条动作。**四个字段全是纯 ASCII**（[`ascii_token`](crate::restore::ascii_token) 保证）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedAction {
    /// 这一行在快照里的稳定身份（`tools:node` / `user:12` / `env:user:JAVA_HOME` / `wsl:Ubuntu`）。
    pub id: String,
    /// 动作类别的稳定 slug（`install` / `add` / `set-machine` / `skipped-secret` / …）。
    pub kind: String,
    /// 这条动作作用在**什么**上（工具名 / 环境变量名 / `PATH` 那一行的值）。
    pub subject: String,
    /// 稳定的补充数据（`recipe=node version=24.19.0` / `toIndex=16` / `scope=machine`）。
    pub detail: String,
}

impl PlannedAction {
    /// 造一条。`id` 由调用方给（它是各行自己的身份），`subject` 过一遍
    /// [`ascii_token`](crate::restore::ascii_token)，`detail` 过一遍保留 `key=value`
    /// 结构的那个版本（`scope=machine var=PATH` 里的 `=` 不许被打掉）。
    #[must_use]
    pub fn new(id: String, kind: &str, subject: &str, detail: impl AsRef<str>) -> Self {
        Self {
            id,
            kind: kind.to_owned(),
            subject: super::manual::ascii_token(subject),
            detail: super::manual::ascii_layout(detail.as_ref()),
        }
    }
}

/// 一节的条数，**两个口径**（决策 158）。
///
/// * `rows` —— **分类**的行数：`PATH` 里有几条 `add`、本机有几条工具已经装好。
///   **每一节的键全部出现（含 0）**，形状稳定，GUI 可以无条件取键。
/// * `effective` —— **真会写的条数**。真机实测目标侧重复两条 → `rows.add == 2`
///   而 `effective.add == 1`。**只出现非零的键**：全空就是"这一节什么都不写"，
///   比一张全是 0 的表更不容易被读反。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionCounts {
    /// 分类口径。
    pub rows: BTreeMap<String, u64>,
    /// 生效口径。
    pub effective: BTreeMap<String, u64>,
}

impl SectionCounts {
    /// 一张把 `keys` 全部置 0 的 `rows` 表（`effective` 从空开始）。
    #[must_use]
    pub fn with_keys(keys: &[&str]) -> Self {
        let mut rows = BTreeMap::new();
        for key in keys {
            rows.insert((*key).to_owned(), 0);
        }
        Self {
            rows,
            effective: BTreeMap::new(),
        }
    }

    /// `rows[key] += 1`（键必须已经在 [`Self::with_keys`] 里）。
    pub(crate) fn bump_row(&mut self, key: &str) {
        *self.rows.entry(key.to_owned()).or_insert(0) += 1;
    }

    /// `rows[key] = value`。
    pub(crate) fn set_row(&mut self, key: &str, value: u64) {
        self.rows.insert(key.to_owned(), value);
    }

    /// `effective[key] += 1`。
    pub(crate) fn bump_effective(&mut self, key: &str) {
        *self.effective.entry(key.to_owned()).or_insert(0) += 1;
    }

    /// `effective` 的总条数。
    #[must_use]
    pub fn effective_total(&self) -> u64 {
        self.effective.values().sum()
    }
}

/// 一个 section 的计划。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionPlan {
    /// 哪一节。
    pub id: SectionId,
    /// **最需要用户注意的那一件事**（决策 161）。
    pub status: SectionStatus,
    /// 两个口径的条数。
    pub counts: SectionCounts,
    /// 这一节要做的动作（全 ASCII）。
    pub actions: Vec<PlannedAction>,
    /// 这一节要下载才能做（与 `status` **正交**）。
    pub needs_network: bool,
    /// 这一节有机器级的写入 —— 要用户以管理员身份做（与 `status` **正交**）。
    pub requires_elevation: bool,
    /// 一句稳定 slug 的说明（`fix-not-selected` / `report-only` / …）。**"什么都没有"不写 `Some("")`。**
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl SectionPlan {
    /// 一节压根没参与（没被选中 / 快照里没有它）。
    #[must_use]
    pub fn skipped(id: SectionId, note: &'static str) -> Self {
        Self {
            id,
            status: SectionStatus::Skipped,
            counts: SectionCounts::default(),
            actions: Vec::new(),
            needs_network: false,
            requires_elevation: false,
            note: Some(note.to_owned()),
        }
    }
}

/// 整份计划的摘要。**六个计数器都是 `status` 的直方图**（六个数相加 = `sections`）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSummary {
    /// 计划里有几节（**永远是四节**：没选中的也在，记 `skipped`）。
    pub sections: u64,
    /// `no-change` 的节数。
    pub no_change: u64,
    /// `would-change` 的节数。
    pub would_change: u64,
    /// `requires-elevation` 的节数。
    pub requires_elevation: u64,
    /// `needs-network` 的节数（要下载的节 —— 决策 157 的"在 summary 里列出"）。
    pub needs_network: u64,
    /// `unsupported` 的节数。
    pub unsupported: u64,
    /// `skipped` 的节数。
    pub skipped: u64,
    /// 手动待办的总条数。
    pub manual_actions: u64,
}

impl RestoreSummary {
    /// 数出来。
    #[must_use]
    pub fn of(sections: &[SectionPlan], manual_actions: usize) -> Self {
        let mut summary = Self {
            sections: sections.len() as u64,
            manual_actions: manual_actions as u64,
            ..Self::default()
        };
        for section in sections {
            let slot = match section.status {
                SectionStatus::NoChange => &mut summary.no_change,
                SectionStatus::WouldChange => &mut summary.would_change,
                SectionStatus::RequiresElevation => &mut summary.requires_elevation,
                SectionStatus::NeedsNetwork => &mut summary.needs_network,
                SectionStatus::Unsupported => &mut summary.unsupported,
                SectionStatus::Skipped => &mut summary.skipped,
            };
            *slot += 1;
        }
        summary
    }
}

/// 一份完整的计划。**它就是 `--json` 里 `data` 的那四个键**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    /// **用户敲的那个路径原样**（决策 162）。来源见 [`RestoreBundle::source`]。
    pub snapshot: String,
    /// 四节，**顺序固定为 [`SectionId::ALL`]**（两次调用逐条相同）。
    pub sections: Vec<SectionPlan>,
    /// 做不到的事，逐条带具体原因。
    pub manual_actions: Vec<ManualAction>,
    /// 摘要。
    pub summary: RestoreSummary,
}

impl RestorePlan {
    /// 有**任何**一节会被写吗。
    ///
    /// 判据是 `status ∈ {would-change, requires-elevation, needs-network}`（决策 161）。
    /// `unsupported` / `no-change` / `skipped` **不算** —— 那几种情形一个字节都不会写
    /// （但它们的 `manual_actions` 照旧要打印，那是 CLI 的事）。
    #[must_use]
    pub fn has_changes(&self) -> bool {
        self.sections
            .iter()
            .any(|section| section.status.is_change())
    }

    /// 取某一节的计划。
    #[must_use]
    pub fn section(&self, id: SectionId) -> Option<&SectionPlan> {
        self.sections.iter().find(|section| section.id == id)
    }
}

/// 算出"照着这份快照还原本机"的计划。**纯函数**。
///
/// 五节**永远都在**（顺序固定）：
///
/// * 没被 `--only` 选中的 → `skipped` + `note = "not-selected"`（决策 161）；
/// * 快照里根本没有的 → `skipped` + `note = "section-not-in-snapshot"`（决策 159）。
///   这是**可见的说明，不是错误退出** —— 用户的下一步完全不同（"你没让我做这一节"
///   vs "你给我的快照里没有这一节"）。
///
/// # 空快照
///
/// 一份四个 section 文件都没有的目录**到不了这里**：[`RestoreBundle::load`] 已经用
/// `empty-snapshot` 拒绝了它。原因是四节全 `skipped` 的计划**看起来像"本机全都对上了"**，
/// 而那份快照其实什么都没说 —— 静默成功正是这一票要消灭的东西。
#[must_use]
pub fn plan(target: &RestoreBundle, local: &RestoreBundle, opts: &RestoreOptions) -> RestorePlan {
    let requested = opts.effective_sections();
    let mut section_plans: Vec<SectionPlan> = Vec::with_capacity(SectionId::ALL.len());
    let mut manual_actions: Vec<ManualAction> = Vec::new();

    for id in SectionId::ALL {
        if !requested.contains(&id) {
            section_plans.push(SectionPlan::skipped(id, NOTE_NOT_SELECTED));
            continue;
        }
        // 目标侧缺这一节 → 一句可见的说明（决策 159），不是错误。
        let outcome = match id {
            SectionId::Tools => target
                .tools
                .as_ref()
                .map(|file| sections::tools(file, local.tools.as_ref())),
            SectionId::Path => target
                .path
                .as_ref()
                .map(|file| sections::path(file, local.path.as_ref(), opts)),
            SectionId::Env => target
                .env
                .as_ref()
                .map(|file| sections::env(file, local.env.as_ref(), target.skipped.as_ref())),
            SectionId::Wsl => target
                .wsl
                .as_ref()
                .map(|file| sections::wsl(file, local.wsl.as_ref())),
            SectionId::Globals => target
                .globals
                .as_ref()
                .map(|file| sections::globals(file, local.globals.as_ref(), opts)),
        };
        match outcome {
            Some((section_plan, manual)) => {
                section_plans.push(section_plan);
                manual_actions.extend(manual);
            }
            None => section_plans.push(SectionPlan::skipped(id, NOTE_SECTION_NOT_IN_SNAPSHOT)),
        }
    }

    let summary = RestoreSummary::of(&section_plans, manual_actions.len());
    let snapshot = target
        .source
        .clone()
        .or_else(|| target.captured_at().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned());

    RestorePlan {
        snapshot,
        sections: section_plans,
        manual_actions,
        summary,
    }
}

#[cfg(test)]
mod tests {
    use tuoen_platform::EnvScope;

    use super::*;
    use crate::capture::Existence;
    use crate::restore::test_support as fixture;

    /// 真机形状的一对 bundle：**两侧一模一样**（这就是"还原到本机"）。
    ///
    /// 数字取自 Lead 用真机完整快照算出来的那一份：`tools.toml` 29 行
    /// （17 条可复现 / 12 条不可复现）、`manager` 出现 2 次（`nvm4w`、`uv`）、
    /// `env.toml` 32 行、`wsl.toml` 2 个发行版、`skipped.toml` 1 条
    /// （`ARK_API_KEY`，`credential-named`）。
    fn real_machine() -> RestoreBundle {
        let mut tools = Vec::new();
        for index in 0..17 {
            tools.push(fixture::tool(
                &format!("reproducible-{index:02}"),
                Some("1.0.0"),
                None,
                true,
            ));
        }
        tools.push(fixture::tool("node", Some("24.19.0"), Some("nvm4w"), false));
        tools.push(fixture::tool("python", Some("3.13.0"), Some("uv"), false));
        for index in 0..10 {
            tools.push(fixture::tool(
                &format!("unreproducible-{index:02}"),
                Some("2.0.0"),
                None,
                false,
            ));
        }
        assert_eq!(tools.len(), 29);

        let mut env_rows: Vec<(EnvScope, &str, &str)> = Vec::new();
        for index in 0..13 {
            env_rows.push((
                EnvScope::User,
                Box::leak(format!("USER_VAR_{index:02}").into_boxed_str()),
                r"C:\x",
            ));
        }
        for index in 0..19 {
            env_rows.push((
                EnvScope::Machine,
                Box::leak(format!("MACHINE_VAR_{index:02}").into_boxed_str()),
                r"C:\y",
            ));
        }
        assert_eq!(env_rows.len(), 32);

        // 本机 PATH 自己的病：24 条 fix（真机数字）。目标是**同一份文件** ——
        // 那 24 条是本机自己的问题，不是"与快照的差异"，所以默认只报告、不选中。
        let mut path_rows: Vec<(EnvScope, &str, Existence)> = Vec::new();
        for index in 0..12 {
            path_rows.push((
                EnvScope::Machine,
                Box::leak(format!(r"C:\machine\{index:02}").into_boxed_str()),
                Existence::Yes,
            ));
        }
        for index in 0..24 {
            path_rows.push((
                EnvScope::User,
                Box::leak(format!(r"C:\user\{index:02}").into_boxed_str()),
                Existence::No,
            ));
        }
        let path = fixture::path(&path_rows);

        fixture::bundle()
            .source_of("tuoen.d")
            .tools(fixture::tools_file(tools))
            .path(path)
            .env(fixture::env(&env_rows))
            .wsl(fixture::wsl(&[
                ("Ubuntu-22.04", r"C:\Users\a\AppData\Local\WSL\Ubuntu-22.04"),
                ("Arch-Linux-current", r"C:\linux\Arch-Linux-current"),
            ]))
            .skipped(fixture::skipped(&[(
                "env",
                "ARK_API_KEY",
                Some("user"),
                "credential-named",
            )]))
            .build()
    }

    #[test]
    fn restoring_to_the_machine_itself_produces_a_near_empty_plan() {
        let bundle = real_machine();
        let plan = plan(&bundle, &bundle, &RestoreOptions::all());

        assert_eq!(plan.sections.len(), 5);
        for section in &plan.sections {
            if section.id == SectionId::Globals {
                // 这份固定装置里**没有** `globals.toml` ⇒ 如实说"快照里没有它"
                // （决策 159）。它与"本机全对上了"不是同一句话，所以这里不能
                // 顺手把 `Globals` 也当成 `NoChange`。
                assert_eq!(section.status, SectionStatus::Skipped);
                assert_eq!(section.note.as_deref(), Some(NOTE_SECTION_NOT_IN_SNAPSHOT));
                continue;
            }
            assert_eq!(
                section.status,
                SectionStatus::NoChange,
                "{} 这一节不该有变更：{:?}",
                section.id.as_str(),
                section.counts
            );
        }
        assert!(!plan.has_changes(), "票据的判据：还原到本机 = 接近空的计划");
        assert!(plan.summary.no_change == 4);
        assert!(plan.summary.skipped == 1, "globals 那一节不在固定装置里");
        // "接近空"不是"一行动作都没有"：唯一的动作是**报告**性质的 `skipped-secret`
        // （本机 32 个变量全都在，但那 1 条凭据我们没捕获过，要说出来）。
        assert!(
            plan.sections
                .iter()
                .flat_map(|section| &section.actions)
                .all(|action| action.kind == "skipped-secret"),
            "{:?}",
            plan.sections
                .iter()
                .flat_map(|section| &section.actions)
                .collect::<Vec<_>>()
        );
        assert!(
            plan.sections
                .iter()
                .all(|section| section.counts.effective.is_empty()),
            "一个字节都不会写"
        );
        assert_eq!(
            plan.section(SectionId::Path).expect("path").counts.rows["fix"],
            24,
            "本机 PATH 自己的 24 条病要报出来，但默认不动它们"
        );

        // 但 `manual_actions` **预期非空**：1 条凭据 + 2 条管理器。
        assert_eq!(plan.summary.manual_actions, 3);
        let codes: Vec<&str> = plan
            .manual_actions
            .iter()
            .map(|m| m.code.as_str())
            .collect();
        assert_eq!(
            codes,
            [
                "third-party-manager",
                "third-party-manager",
                "credential-reconfigure"
            ]
        );
        let subjects: Vec<&str> = plan
            .manual_actions
            .iter()
            .map(|m| m.subject.as_str())
            .collect();
        assert_eq!(subjects, ["nvm4w", "uv", "env:ARK_API_KEY"]);
        // 12 条不可复现的工具**在这一节里什么都不缺**（本机都有），
        // 所以一条 `unsupported` 噪声都不该出现。
        assert!(
            !plan
                .manual_actions
                .iter()
                .any(|m| m.code == crate::restore::ManualActionCode::Unsupported),
            "本机自己那份快照不该产出 unsupported 噪声：{:?}",
            plan.manual_actions
        );
    }

    #[test]
    fn the_summary_is_a_status_histogram() {
        // ① 本机就是快照 → 四节全 no-change。
        let same = real_machine();
        let plan = plan(&same, &same, &RestoreOptions::all());
        let summary = plan.summary;
        assert_eq!(
            summary.no_change
                + summary.would_change
                + summary.requires_elevation
                + summary.needs_network
                + summary.unsupported
                + summary.skipped,
            summary.sections,
            "六个数相加必须等于 sections"
        );

        // ② 换一台空机器 → 每一节都变成有事的形态，直方图照样自洽。
        let empty = fixture::bundle()
            .tools(fixture::tools(&[]))
            .path(fixture::path(&[]))
            .env(fixture::env(&[]))
            .wsl(fixture::wsl(&[]))
            .build();
        let plan = super::plan(&same, &empty, &RestoreOptions::all());
        assert!(plan.has_changes());
        assert_eq!(plan.summary.needs_network, 1, "tools 要下载");
        assert_eq!(
            plan.summary.would_change + plan.summary.requires_elevation,
            2,
            "path 与 env"
        );
        assert_eq!(plan.summary.no_change, 1, "wsl 只报告");
        let summary = plan.summary;
        assert_eq!(
            summary.no_change
                + summary.would_change
                + summary.requires_elevation
                + summary.needs_network
                + summary.unsupported
                + summary.skipped,
            summary.sections
        );
    }

    #[test]
    fn a_section_can_need_network_and_elevation_at_the_same_time() {
        // path 是机器级改动（要提权），tools 要下载 —— 两件事在两个 bool 上各自为真。
        let target = fixture::bundle()
            .tools(fixture::tools(&[("node", Some("24.19.0"), None, true)]))
            .path(fixture::path(&[(
                EnvScope::Machine,
                r"C:\Dev",
                Existence::Yes,
            )]))
            .env(fixture::env(&[(
                EnvScope::Machine,
                "NVM_HOME",
                r"C:\nvm4w",
            )]))
            .wsl(fixture::wsl(&[]))
            .build();
        let local = fixture::bundle()
            .tools(fixture::tools(&[]))
            .path(fixture::path(&[(
                EnvScope::Machine,
                r"C:\Windows",
                Existence::Yes,
            )]))
            .env(fixture::env(&[]))
            .wsl(fixture::wsl(&[]))
            .build();
        let plan = plan(&target, &local, &RestoreOptions::all());

        let tools = plan.section(SectionId::Tools).expect("tools");
        let path = plan.section(SectionId::Path).expect("path");
        let env = plan.section(SectionId::Env).expect("env");
        assert!(tools.needs_network && !tools.requires_elevation);
        assert!(!path.needs_network && path.requires_elevation);
        assert!(!env.needs_network && env.requires_elevation);
        assert_eq!(tools.status, SectionStatus::NeedsNetwork);
        assert_eq!(path.status, SectionStatus::RequiresElevation);
        assert_eq!(env.status, SectionStatus::RequiresElevation);
        assert!(plan.has_changes());
    }

    #[test]
    fn the_snapshot_field_is_the_source_verbatim() {
        // 决策 162：计划里的 `snapshot` 就是**用户敲的那个路径原样**。
        let bundle = fixture::bundle()
            .source_of(r".\backup\tuoen.d\")
            .tools(fixture::tools(&[]))
            .build();
        let plan = plan(&bundle, &bundle, &RestoreOptions::all());
        assert_eq!(plan.snapshot, r".\backup\tuoen.d\");
    }

    #[test]
    fn a_bundle_without_a_source_falls_back_to_its_own_timestamp() {
        // 本机侧那个内存 bundle 没有"从哪来"；目标侧是一份手造的、没有 source 的 bundle。
        let target = fixture::bundle().tools(fixture::tools(&[])).build();
        assert_eq!(target.source, None);
        let plan = plan(&target, &fixture::bundle().build(), &RestoreOptions::all());
        assert_eq!(plan.snapshot, fixture::CAPTURED_AT);
    }

    #[test]
    fn every_section_not_selected_is_skipped_and_says_which_reason() {
        // 决策 161：被 `--only` 排除的 → skipped + not-selected。
        let bundle = real_machine();
        let opts = RestoreOptions {
            sections: vec![SectionId::Path],
            ..RestoreOptions::all()
        };
        let plan = plan(&bundle, &bundle, &opts);
        assert_eq!(
            plan.sections.len(),
            5,
            "没选中的也在计划里（用户要看得出他漏了什么）"
        );
        assert_eq!(
            plan.section(SectionId::Path).expect("path").status,
            SectionStatus::NoChange
        );
        for id in [
            SectionId::Tools,
            SectionId::Env,
            SectionId::Wsl,
            SectionId::Globals,
        ] {
            let section = plan.section(id).expect("skipped");
            assert_eq!(section.status, SectionStatus::Skipped);
            assert_eq!(section.note.as_deref(), Some("not-selected"));
            assert!(section.counts.rows.is_empty(), "没做过的事不许有条数");
        }
        assert_eq!(plan.summary.skipped, 4);
    }

    #[test]
    fn a_section_missing_from_the_snapshot_is_skipped_not_an_error() {
        // 决策 159：`--only` 指向快照里没有的 section → skipped + 说明，**不是错误退出**。
        let target = fixture::bundle()
            .tools(fixture::tools(&[("node", Some("24.19.0"), None, true)]))
            .build();
        let local = fixture::bundle().build();
        let plan = plan(&target, &local, &RestoreOptions::all());
        assert_eq!(
            plan.section(SectionId::Tools).expect("tools").status,
            SectionStatus::NeedsNetwork
        );
        for id in [
            SectionId::Path,
            SectionId::Env,
            SectionId::Wsl,
            SectionId::Globals,
        ] {
            let section = plan.section(id).expect("skipped");
            assert_eq!(section.status, SectionStatus::Skipped);
            assert_eq!(section.note.as_deref(), Some("section-not-in-snapshot"));
        }
        assert_eq!(plan.summary.skipped, 4);
        // 两份说明必须分得开 —— 用户看到的话完全不同。
        assert_ne!(NOTE_NOT_SELECTED, NOTE_SECTION_NOT_IN_SNAPSHOT);
    }

    #[test]
    fn requesting_a_section_that_is_not_in_the_snapshot_is_not_an_error() {
        // 显式 `--only env`，而快照里没有 env.toml。
        let target = fixture::bundle().tools(fixture::tools(&[])).build();
        let opts = RestoreOptions {
            sections: vec![SectionId::Env],
            ..RestoreOptions::all()
        };
        let plan = plan(&target, &fixture::bundle().build(), &opts);
        let env = plan.section(SectionId::Env).expect("env");
        assert_eq!(env.status, SectionStatus::Skipped);
        assert_eq!(env.note.as_deref(), Some("section-not-in-snapshot"));
        assert_eq!(plan.summary.skipped, 5, "另外四节是 not-selected");
    }

    #[test]
    fn sections_are_always_in_the_fixed_order() {
        let target = real_machine();
        let plan = plan(&target, &target, &RestoreOptions::all());
        let ids: Vec<&str> = plan.sections.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["tools", "path", "env", "wsl", "globals"]);
        // 反复算两次逐条相同（`--json` 必须逐字节稳定）。
        let again = super::plan(&target, &target, &RestoreOptions::all());
        assert_eq!(plan, again);
    }

    #[test]
    fn unsupported_alone_is_not_a_change() {
        let target = fixture::bundle()
            .tools(fixture::tools(&[("weird", Some("1"), None, false)]))
            .build();
        let plan = plan(&target, &fixture::bundle().build(), &RestoreOptions::all());
        let tools = plan.section(SectionId::Tools).expect("tools");
        assert_eq!(tools.status, SectionStatus::Unsupported);
        assert!(
            !plan.has_changes(),
            "做不到的事不等于'要改'（但它照旧要打印）"
        );
        assert_eq!(plan.manual_actions.len(), 1);
    }

    #[test]
    fn an_empty_request_means_every_section() {
        let opts = RestoreOptions::all();
        assert_eq!(opts.effective_sections(), SectionId::ALL.to_vec());
    }

    #[test]
    fn requested_sections_are_deduped_and_kept_in_the_fixed_order() {
        let opts = RestoreOptions {
            sections: vec![SectionId::Wsl, SectionId::Tools, SectionId::Wsl],
            ..RestoreOptions::all()
        };
        assert_eq!(
            opts.effective_sections(),
            vec![SectionId::Tools, SectionId::Wsl],
            "顺序稳定才能比逐条相同"
        );
    }

    #[test]
    fn the_whole_plan_is_pure_ascii() {
        // 契约：成功载荷必须纯 ASCII（中文只进人类输出）。
        let mut row = fixture::tool("cargo", Some("1.88.0"), None, true);
        row.evidence = "PATH 第 1 条（process-only）里有 cargo.exe".to_owned();
        row.path = "<无 InstallLocation，卸载键 {GUID}>".to_owned();
        let target = fixture::bundle()
            .source_of(r"C:\快照\tuoen.d")
            .tools(fixture::tools_file(vec![
                row,
                fixture::tool("中文工具", Some("1.0"), Some("管理器"), false),
            ]))
            .path(fixture::path(&[(
                EnvScope::User,
                r"C:\工具\bin",
                Existence::Yes,
            )]))
            .env(fixture::env(&[(EnvScope::User, "中文变量", r"C:\值")]))
            .wsl(fixture::wsl(&[("发行版", r"C:\wsl\发行版")]))
            .skipped(fixture::skipped(&[(
                "env",
                "中文凭据",
                Some("user"),
                "credential-named",
            )]))
            .build();
        let plan = plan(&target, &fixture::bundle().build(), &RestoreOptions::all());

        // `snapshot` 是**用户敲的路径原样**，它不归 `ascii_token` 管（决策 162：
        // 用户敲什么就印什么）—— 所以把它换成空串之后，**整份载荷**必须还是纯 ASCII。
        let mut data = serde_json::to_value(&plan).expect("序列化");
        data["snapshot"] = serde_json::Value::String(String::new());
        let text = serde_json::to_string(&data).expect("序列化");
        assert!(
            text.is_ascii(),
            "成功载荷必须纯 ASCII（`snapshot` 除外），实际：{text}"
        );
        assert!(text.contains("manualActions"), "{text}");
        assert!(text.contains("subject"), "{text}");
        for section in &plan.sections {
            for action in &section.actions {
                for text in [&action.id, &action.kind, &action.subject, &action.detail] {
                    assert!(text.is_ascii(), "动作里有非 ASCII：{text}");
                }
            }
        }
        for action in &plan.manual_actions {
            for text in [&action.subject, &action.detail, &action.remediation] {
                assert!(text.is_ascii(), "待办里有非 ASCII：{text}");
            }
        }
    }

    #[test]
    fn the_json_shape_is_camel_case_and_kebab_enums() {
        let target = fixture::bundle()
            .tools(fixture::tools(&[("node", Some("24.19.0"), None, true)]))
            .build();
        let plan = plan(&target, &fixture::bundle().build(), &RestoreOptions::all());
        let json: serde_json::Value = serde_json::to_value(&plan).expect("序列化");
        for key in ["snapshot", "sections", "manualActions", "summary"] {
            assert!(json.get(key).is_some(), "顶层少了 {key}：{json}");
        }
        let section = &json["sections"][0];
        assert_eq!(section["id"], "tools");
        assert_eq!(section["status"], "needs-network");
        assert_eq!(section["needsNetwork"], true);
        assert_eq!(section["requiresElevation"], false);
        assert!(
            section.get("note").is_none(),
            "None 不许变成 null（skip_serializing_if）"
        );
        assert_eq!(section["counts"]["rows"]["missing"], 1);
        assert_eq!(section["counts"]["effective"]["install"], 1);
        assert_eq!(json["summary"]["manualActions"], 0);

        // `note` 有值时才出现。
        let missing = fixture::bundle().tools(fixture::tools(&[])).build();
        let plan = super::plan(&missing, &fixture::bundle().build(), &RestoreOptions::all());
        let json: serde_json::Value = serde_json::to_value(&plan).expect("序列化");
        assert_eq!(json["sections"][1]["note"], "section-not-in-snapshot");
    }
}
