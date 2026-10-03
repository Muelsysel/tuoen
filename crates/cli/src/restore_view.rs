//! `tuoen restore` 的 `--json` 视图与人类输出（票据 #16）。
//!
//! ## 两个出口，一份事实
//!
//! `data` 就是 core 的 [`RestorePlan`] **原样**（`#[serde(flatten)]`，一个字段都不
//! 重排、不改名、不加时间戳），加上一个**只在 `--apply` 时出现**的 `apply` 对象。
//! 计划的四个键（`snapshot` / `sections` / `manualActions` / `summary`）在两种模式
//! 下逐字节相同 —— 这是决策 150 的"计划与执行是同一份东西"在 JSON 上的样子。
//!
//! 为什么 `apply` 是 CLI 侧的类型而不是 core 的：core 的 [`SectionStatus`] 描述的是
//! **计划**（"这个 section 会怎样"），而 `apply` 描述的是**这次跑完的结果**
//! （"它到底写没写、广播回执几条、错在哪"）。前者是纯函数的输出，后者只有在真的
//! 动过手之后才存在。
//!
//! ## 人类输出为什么这么啰嗦
//!
//! `restore` 会改用户的环境。所以每一段都要回答"你到底动了什么、没动什么、为什么"：
//! 计划逐条、预算、机器级为什么不写、`--only` 排除的与快照里没有的**分开说**、
//! 以及**人工待办无条件打印** —— "无变更"不等于"你什么都不用做"（决策 161 的推论）。

use std::collections::BTreeMap;

use serde::Serialize;
use tuoen_core::restore::{
    ManualAction, NOTE_FIX_NOT_SELECTED, NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION, NOTE_NEEDS_NETWORK,
    NOTE_NOT_SELECTED, NOTE_REPORT_ONLY, NOTE_SECTION_NOT_IN_SNAPSHOT,
    REMEDIATION_INSTALL_MANUALLY, REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION,
    REMEDIATION_RECONFIGURE_MANUALLY, REMEDIATION_RUN_AS_ADMINISTRATOR,
    REMEDIATION_USE_THE_MANAGER, RestorePlan, RestoreSummary, SectionCounts, SectionId,
    SectionPlan,
};

use crate::path_diff_cmd::Username;

/// `--json` 的 `data`：计划那四个键 + **一个 CLI 侧拥有的键** + 可选的结果。
///
/// # 为什么这里不是 `#[serde(flatten)] plan` 了
///
/// 票据 #17（决策 183）要求在 `summary` 里加 `unrestorable`，而那个键**不在** core 的
/// [`RestoreSummary`] 里（写入范围不含 `crates/core/**`）。`#[serde(flatten)]` 铺开的
/// 是 core 的整个 `RestorePlan`，**不允许覆盖其中一个已存在的键** —— 所以这里把四个键
/// 逐个引用过来（**不重算、不重命名**），只把 `summary` 包一层。
///
/// 代价说清楚：core 以后给 `RestorePlan` 加第五个键时，这里**不会**自动跟上。
/// 那一天的正确做法是把 `unrestorable` 挪进 core 的 `RestoreSummary`，然后退回 flatten。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RestoreDataView<'a> {
    /// core 的 `RestorePlan::snapshot` 原样（**不要**在这里重新拼）。
    pub(crate) snapshot: &'a str,
    pub(crate) sections: &'a [SectionPlan],
    pub(crate) manual_actions: &'a [ManualAction],
    pub(crate) summary: SummaryView<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) apply: Option<&'a ApplyReport>,
}

/// `summary`：core 的六个计数器 + CLI 侧的 `unrestorable`。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SummaryView<'a> {
    #[serde(flatten)]
    pub(crate) summary: &'a RestoreSummary,
    /// 快照里有、而 L1 的 `restore` **不还原**的 section。
    ///
    /// **只在非空时出键**：一份没有 `globals` / `configs` 的快照，这个键根本不存在 ——
    /// `[]` 会被读成"我看过了，一个都没有"，而那份快照根本没说过这件事。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) unrestorable: Vec<String>,
}

/// `--apply` 这一次跑完的结果。
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyReport {
    /// 有没有任何一个 section 真的落了盘 / 落了注册表。
    pub(crate) wrote: bool,
    pub(crate) sections: Vec<SectionOutcome>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) failures: Vec<ApplyFailure>,
}

/// 一个 section 的实际结局。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SectionOutcome {
    pub(crate) id: &'static str,
    /// `applied` / `no-change` / `skipped` / `report-only` / `requires-elevation` /
    /// `needs-network` / `failed`。
    pub(crate) outcome: &'static str,
    pub(crate) wrote: bool,
    /// 广播回执条数 —— 只有真的写过环境块才有意义。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) broadcast_replies: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<ApplyFailure>,
}

/// 一处没做成。**全 ASCII**：成功载荷要能逐字节比对，中文只进人类输出。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplyFailure {
    pub(crate) code: &'static str,
    pub(crate) detail: String,
}

impl ApplyReport {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// 记一个 section 的结局，并把它的**全部**失败同步进顶层 `failures`。
    ///
    /// `outcome.error` 只留第一条（一个 section 一次只能说一件事），而顶层那份是
    /// 全量 —— 退出码看的是"有没有失败"，不是"失败了几条"。
    pub(crate) fn push(&mut self, outcome: SectionOutcome, failures: Vec<ApplyFailure>) {
        self.failures.extend(failures);
        self.wrote |= outcome.wrote;
        self.sections.push(outcome);
    }

    pub(crate) fn failed(&self) -> bool {
        !self.failures.is_empty()
    }

    pub(crate) fn outcome(&self, id: SectionId) -> Option<&SectionOutcome> {
        self.sections.iter().find(|s| s.id == id.as_str())
    }
}

// ---------------------------------------------------------------- 人类输出

/// 打印计划（`--dry-run` / 默认形态）。
pub(crate) fn print_plan_human(
    plan: &RestorePlan,
    username: &Username,
    unrestorable: &[String],
    apply: Option<&ApplyReport>,
) {
    let applying = apply.is_some();
    if applying {
        println!("恢复结果 —— 快照：{}", plan.snapshot);
        println!("模式：真的动手（`--apply`）。");
    } else {
        println!("恢复计划 —— 快照：{}", plan.snapshot);
        println!(
            "模式：只出计划（**什么都没有写**）。要真的做：`tuoen restore {} --apply`",
            plan.snapshot
        );
    }
    println!();

    for section in &plan.sections {
        print_section(section, username, apply.and_then(|a| a.outcome(section.id)));
    }

    print_summary(plan);
    print_unrestorable(unrestorable);
    print_manual_actions(&plan.manual_actions);

    if let Some(report) = apply {
        print_apply_tail(report);
    } else {
        println!();
        println!("以上是计划，**什么都没有写**。");
    }
}

/// 快照里有、而 L1 的 `restore` **不还原**的 section。
///
/// # 为什么这一行必须存在
///
/// 一份带了 `globals.toml` / `configs.toml` 的快照，在 `restore` 眼里是**四条** section
/// 全 `no-change` —— 于是人类输出会以「这份快照要写的东西：**没有**」结尾。那句话在这里
/// 是**真的**（`restore` 确实什么都不写），但它会被读成「这份快照里的东西本机都有了」，
/// 而真相是「里面有两类东西我压根不还原」。**一句看起来完全合理的错话**正是这一票要消灭的，
/// 所以这一行不是补充说明，它是那句话的限定条件。
fn print_unrestorable(unrestorable: &[String]) {
    if unrestorable.is_empty() {
        return;
    }
    println!();
    println!(
        "注意：这份快照里还有 **{}** —— L1 的 `restore` **不还原**它们（只捕获）。",
        unrestorable.join(" · ")
    );
    println!(
        "  这不是「没看见」：它们的内容在快照里（`globals.toml` / `configs.toml`）。\
         还原它们要跑包管理器自己的解析、或者替用户写配置文件 —— 两件事 L1 都明确不做。"
    );
}

fn print_section(section: &SectionPlan, username: &Username, outcome: Option<&SectionOutcome>) {
    let arrow = match outcome {
        Some(o) => format!(" → {}", outcome_prose(o)),
        None => String::new(),
    };
    println!(
        "[{}] {} · {}{}",
        section.id.as_str(),
        section_prose(section.id),
        status_prose(section.status.as_str()),
        arrow
    );

    let rows = counts_line(&section.counts);
    if !rows.is_empty() {
        println!("  行数：{rows}");
    }
    let effective = effective_line(&section.counts);
    if !effective.is_empty() {
        println!("  {effective}");
    }

    // path 的类会随 `current_username` 变 —— 不说来源，用户会以为工具不稳定。
    if section.id == SectionId::Path {
        println!(
            "  当前用户名：{}（来自{}）—— 同一份快照在不同用户名下类别会不同",
            username.value.as_deref().unwrap_or("取不到"),
            username.source
        );
    }

    // `needsNetwork` / `requiresElevation` 与 `status` **正交**（决策 161）：
    // 一个 `requires-elevation` 的节可能同时要下载。所以它们是**另外两行**，
    // 而不是被塞进 `status` 那句里。
    if section.needs_network {
        println!("  这一节要下载才能做（需要网络）。");
    }
    if section.requires_elevation {
        println!("  这一节有**机器级**写入：要管理员权限，tuoen 不自动提权。");
    }

    for action in &section.actions {
        println!(
            "  · [{}] {} {}（{}）",
            action.id,
            kind_prose(section.id, &action.kind),
            action.subject,
            action.detail
        );
    }

    if let Some(note) = &section.note {
        println!("  说明：{}", note_prose(note));
    }

    if let Some(failure) = outcome.and_then(|o| o.error.as_ref()) {
        println!("  ✗ 没有做：{}（{}）", failure.detail, failure.code);
    }
    println!();
}

/// 一个 section 的计数一行。键的顺序来自 `BTreeMap`（字母序，稳定）。
fn counts_line(counts: &SectionCounts) -> String {
    join_counts(&counts.rows)
}

/// 决策 158 的第二个口径：**真会落地几条**（`fix` 里有一部分是"看着像错、但不改"）。
fn effective_line(counts: &SectionCounts) -> String {
    if counts.effective.is_empty() {
        return String::new();
    }
    let text = join_counts(&counts.effective);
    if text.is_empty() {
        String::new()
    } else {
        format!("真会落地：{text}")
    }
}

fn join_counts(map: &BTreeMap<String, u64>) -> String {
    map.iter()
        .map(|(key, value)| format!("{key} {value}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

fn print_summary(plan: &RestorePlan) {
    let s = &plan.summary;
    println!(
        "摘要：{} 个 section —— 无变更 {} · 会变更 {} · 需要提权 {} · 需要网络 {} · 不支持 {} · 跳过 {}",
        s.sections,
        s.no_change,
        s.would_change,
        s.requires_elevation,
        s.needs_network,
        s.unsupported,
        s.skipped
    );
    if !plan.has_changes() {
        println!("这份快照要写的东西：**没有** —— 本机已经和它一致。");
    }
}

/// **无条件打印**（决策 161 的推论）：`has_changes() == false` 也照样有活要人干。
fn print_manual_actions(actions: &[ManualAction]) {
    if actions.is_empty() {
        return;
    }
    println!();
    println!(
        "人工待办（{} 条）—— **无变更不等于你什么都不用做**：",
        actions.len()
    );
    for action in actions {
        println!(
            "  · [{}] {}：{}",
            manual_code_prose(action.code.as_str()),
            action.subject,
            manual_detail_prose(action.code.as_str(), &action.detail)
        );
        println!("    怎么办：{}", remediation_prose(&action.remediation));
    }
}

fn print_apply_tail(report: &ApplyReport) {
    println!();
    if report.wrote {
        println!("✓ 已经写入的部分在下面逐条列了。");
    } else {
        println!("这次**一个字节都没有写**（计划与现状一致，或者全部需要提权 / 网络）。");
    }
    // 决策 138 要求这几句必须出现：环境块是进程启动时复制的，改注册表不等于改现在。
    println!(
        "**已经跑着的**终端 / IDE / 服务 / 计划任务拿不到新环境（环境块是 `CreateProcess` 时复制的；**我们自己的进程也拿不到**）。"
    );
    println!("所以「**请重启终端**」是平台限制，不是工具不友好。");
}

fn outcome_prose(outcome: &SectionOutcome) -> String {
    let base = match outcome.outcome {
        "applied" => "已写入",
        "no-change" => "无变更",
        "skipped" => "已跳过",
        "report-only" => "只报告",
        "requires-elevation" => "没有写：需要提权",
        "needs-network" => "没有做：需要网络",
        "failed" => "失败",
        other => other,
    };
    match outcome.broadcast_replies {
        Some(replies) => format!("{base}（广播回执 {replies} 条）"),
        None => base.to_string(),
    }
}

/// section 的中文名。
pub(crate) fn section_prose(id: SectionId) -> &'static str {
    match id {
        SectionId::Tools => "工具",
        SectionId::Path => "PATH",
        SectionId::Env => "环境变量",
        SectionId::Wsl => "WSL",
    }
}

fn status_prose(status: &str) -> &str {
    match status {
        "no-change" => "无变更",
        "would-change" => "会变更",
        "requires-elevation" => "需要提权",
        "needs-network" => "需要网络",
        "unsupported" => "不支持",
        "skipped" => "跳过",
        other => other,
    }
}

fn kind_prose(section: SectionId, kind: &str) -> &str {
    match (section, kind) {
        (SectionId::Tools, "install") => "装",
        (SectionId::Tools, "already-installed") => "已装",
        (SectionId::Tools, "third-party-managed") => "第三方管理器在管",
        (SectionId::Tools, "unsupported") => "不支持",
        (SectionId::Path, "add") => "新增",
        (SectionId::Path, "remove") => "删除",
        (SectionId::Path, "move") => "移动",
        (SectionId::Path, "fix") => "修复",
        (SectionId::Path, "case-only") => "仅大小写",
        (SectionId::Path, "keep") => "保留",
        (SectionId::Env, "set-user") => "设置（用户级）",
        (SectionId::Env, "set-machine") => "设置（机器级）",
        (SectionId::Env, "already-present") => "已一致",
        (SectionId::Env, "skipped-secret") => "凭据，跳过",
        (SectionId::Wsl, "missing-distro") => "缺发行版",
        (SectionId::Wsl, "extra-distro") => "多发行版",
        (SectionId::Wsl, "path-differs") => "路径不同",
        _ => kind,
    }
}

/// `note` 是 core 拥有的稳定 slug，中文散文在 CLI 这一层翻。
///
/// 匹配的是 core 的**常量**而不是字面量：slug 改名时编译器会在这里红，
/// 而抄一份字面量只会让人类输出悄悄退回"（见 `--json`）"。
fn note_prose(note: &str) -> &'static str {
    match note {
        NOTE_FIX_NOT_SELECTED => {
            "fix 类（本机自身的健康问题：重复 / 失效 / 空条目 / 用户名）**默认不选中** —— 要一起应用加 `--with-fix`"
        }
        NOTE_SECTION_NOT_IN_SNAPSHOT => "这份快照里**没有**这个 section（不是你没选它）",
        NOTE_NOT_SELECTED => "你**没有选它**（`--only` 把它排除了）",
        NOTE_MACHINE_SCOPE_REQUIRES_ELEVATION => {
            "机器级改动需要管理员权限；**tuoen 不自动提权**（决策 12/136）"
        }
        NOTE_NEEDS_NETWORK => "需要下载（要网络）",
        NOTE_REPORT_ONLY => "只报告，**不写**",
        _ => "（见 `--json`）",
    }
}

fn manual_code_prose(code: &str) -> &str {
    match code {
        "requires-elevation" => "需要提权",
        "credential-reconfigure" => "凭据要自己重配",
        "licence-blocked" => "许可不允许",
        "third-party-manager" => "第三方版本管理器",
        "unsupported" => "不支持",
        other => other,
    }
}

/// `detail` 是**稳定 slug**（决策 160），中文散文在 CLI 这一层翻。
fn manual_detail_prose(code: &str, detail: &str) -> String {
    let known = match (code, detail) {
        ("licence-blocked", "oracle-jdk-redistribution-not-permitted") => Some(
            "Oracle JDK 的许可（BCL）不允许再分发 —— 这是**许可**问题，不是找不到；\
             请从 Oracle 官网自行获取"
                .to_owned(),
        ),
        ("unsupported", "not-reproducible") => {
            Some("这一行不可复现：快照里没留下能重放它的配方（版本 / 来源 / 哈希）".to_owned())
        }
        // 凭据这一类**只有标识、没有材料**：`subject` 是 `env:ARK_API_KEY` 这种名字，
        // `detail` 说的是"为什么把它当成凭据"。
        ("credential-reconfigure", "credential-named") => Some(
            "变量名里有 `KEY` / `TOKEN` / `SECRET` 这类词，且值够长、不像路径 —— \
             无法排除它是凭据，所以**它没有进快照**（材料从不进快照）。"
                .to_owned(),
        ),
        ("credential-reconfigure", _) => Some(
            "这一条是凭据：tuoen **不搬运凭据材料**（DPAPI / 凭据管理器是用户+机器绑定的，\
             搬过去会静默失败），只记下「需要哪个」。"
                .to_owned(),
        ),
        ("third-party-manager", manager) => Some(format!(
            "由第三方版本管理器 `{manager}` 管 —— tuoen **不接管**它：改它的符号链接要管理员，\
             而且两个管理器会抢同一个 reparse point。"
        )),
        ("requires-elevation", detail) => Some(format!(
            "机器级的写操作（`{detail}`）要管理员权限 —— **tuoen 不自动提权**（决策 12/136）。"
        )),
        _ => None,
    };
    match known {
        Some(text) => text,
        // 不认识的 slug 就原样打出来 —— 编一句像样的话比留一个 slug 更坏。
        None => format!("{detail}（`--json` 里就是这个 slug）"),
    }
}

/// `remediation` 也是 core 的稳定 slug（五个常量），中文在 CLI 这一层翻。
fn remediation_prose(remediation: &str) -> &str {
    match remediation {
        REMEDIATION_RUN_AS_ADMINISTRATOR => "用管理员身份重跑一次",
        REMEDIATION_INSTALL_MANUALLY => "去上游官网自己下载安装（我们不许再分发它）",
        REMEDIATION_USE_THE_MANAGER => "用它自己的命令升级（tuoen 不接管）",
        REMEDIATION_RECONFIGURE_MANUALLY => "自己把这个凭据重新配一遍",
        REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION => "等这个版本支持（我们明确不做，不是失败）",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_core::restore::ManualActionCode;

    #[test]
    fn every_manual_code_has_chinese_prose() {
        // 五类一个都不能漏（漏了就会把 slug 原样打给用户）。取值表来自 core，
        // 所以 core 加第六类时这条会红。
        for code in ManualActionCode::ALL {
            assert_ne!(
                manual_code_prose(code.as_str()),
                code.as_str(),
                "{} 缺中文",
                code.as_str()
            );
        }
    }

    #[test]
    fn every_remediation_has_chinese_prose() {
        for slug in [
            REMEDIATION_RUN_AS_ADMINISTRATOR,
            REMEDIATION_RECONFIGURE_MANUALLY,
            REMEDIATION_INSTALL_MANUALLY,
            REMEDIATION_USE_THE_MANAGER,
            REMEDIATION_NOT_SUPPORTED_IN_THIS_VERSION,
        ] {
            assert_ne!(remediation_prose(slug), slug, "{slug} 缺中文");
        }
    }

    #[test]
    fn the_two_skipped_notes_say_two_different_things() {
        // 决策 161 的补充：`--only` 排除的 vs 快照里没有的，**不是同一句话**。
        assert_ne!(
            note_prose(NOTE_NOT_SELECTED),
            note_prose(NOTE_SECTION_NOT_IN_SNAPSHOT)
        );
        assert!(note_prose(NOTE_NOT_SELECTED).contains("没有选它"));
        assert!(note_prose(NOTE_SECTION_NOT_IN_SNAPSHOT).contains("没有"));
    }

    #[test]
    fn counts_are_printed_in_a_stable_order() {
        let mut counts = SectionCounts::default();
        counts.rows.insert("keep".into(), 25);
        counts.rows.insert("add".into(), 1);
        assert_eq!(counts_line(&counts), "add 1 · keep 25");
    }
}
