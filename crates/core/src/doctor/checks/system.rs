//! `system.*` 检查 —— 四条**状态报告**。
//!
//! # 这一族与前两族不一样：它们永远各有一条
//!
//! 票据把它们的严重度定成 `info`，而它们问的是"这台机器现在是什么样"，
//! 不是"哪里坏了"：提权与否、Developer Mode 开关、长路径开关、WSL 的 vhdx 在哪。
//! 这些是**背景**，读报告的人要靠它们解释别的发现（例如"为什么 junction 能建而
//! symlink 不能"）。所以：
//!
//! - **不是噪声**：`info` 就是给它们准备的档（"只是噪声的是 info"）；
//! - **按 ID 忽略**：`--json` 的消费者可以只关心 `error`/`warn`（票据原话：
//!   "未来要能按 ID 忽略/聚焦"）；
//! - **反例是"另一个取值"**：`Some(true)` 与 `Some(false)` 是两条不同的结论，
//!   而 `None`（问不出来 / 键不在）是第三条。**三者绝不许合并** ——
//!   "不知道"写成"关着"就是编一个答案，而这个仓库里已经有过两次这种教训
//!   （`exists` 的三态、`target_exists` 的四态）。
//!
//! 唯一按实例拆的是 `system.wsl-nonstandard-path`：每个发行版要用户做的事不同
//! （各自搬各自的 vhdx），而且**标准位置的发行版不该被报**。

use super::super::{Finding, MachineFacts, Severity, ids, sources};

/// 跑这一族的四条检查。
#[must_use]
pub fn run(facts: &MachineFacts) -> Vec<Finding> {
    let mut out = Vec::new();
    elevated(facts, &mut out);
    developer_mode(facts, &mut out);
    long_paths(facts, &mut out);
    wsl_nonstandard_path(facts, &mut out);
    out
}

/// 当前进程有没有管理员权限。`info`。
///
/// 为什么值得报：它决定**这台机器上哪些操作会失败** —— 写 `HKLM`、建 symlink
/// （未提权且未开 Developer Mode 时 `CreateSymbolicLinkW` 会失败）。
/// 本项目的默认工作方式是**不提权**（ADR-0001：用 junction 而不是 symlink），
/// 所以"未提权"不是问题，它是设计前提。
fn elevated(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let (message, evidence) = match facts.system.elevated {
        Some(true) => (
            "当前进程**有**管理员权限。tuoen 不需要提权就能工作（junction 就是为此选的），\
             而提权状态下做的改动会绕过一部分保护 —— 心里有数就好"
                .to_owned(),
            vec!["elevated=true".to_owned()],
        ),
        Some(false) => (
            "当前进程**没有**管理员权限 —— 这是 tuoen 的默认工作方式，不是问题。\
             机器级 `PATH` 与 `HKLM` 的改动做不了，那类操作会明确报错而不是悄悄失败"
                .to_owned(),
            vec!["elevated=false".to_owned()],
        ),
        None => (
            "**问不出来**当前进程有没有管理员权限（读令牌失败）。\
             这不是\"未提权\"，而是\"不知道\""
                .to_owned(),
            vec!["elevated=unknown".to_owned()],
        ),
    };
    out.push(Finding::new(
        ids::SYSTEM_ELEVATED,
        Severity::Info,
        message,
        evidence,
        sources::SYSTEM,
    ));
}

/// Developer Mode 开关。`info`。
///
/// 它决定**symlink 能不能建**：未提权 + 未开 Developer Mode 时，文件与目录的
/// symlink 都失败（本机实测：`CreateSymbolicLinkW` 返回 TRUE 却什么都没建，
/// `GetLastError` 是 1314）。这就是 ADR-0001 用 junction 的原因。
///
/// 值名是**实测确认过**的（`AllowDevelopmentWithoutDevLicense`，票据专门提醒
/// 不要照抄未经验证的注册表路径）：本机这个值**不存在** = 没开。
fn developer_mode(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let (message, evidence) = match facts.system.developer_mode {
        Some(true) => (
            "Developer Mode **已开启** —— 未提权也能建 symlink。\
             但 tuoen 仍然用 junction：它在未开 Developer Mode 的机器上也能工作"
                .to_owned(),
            vec!["developer-mode=true".to_owned()],
        ),
        Some(false) => (
            "Developer Mode **未开启** —— 未提权时建 symlink 会失败（这是设计前提，\
             不是缺陷：tuoen 的版本切换用 junction，不需要它）"
                .to_owned(),
            vec!["developer-mode=false".to_owned()],
        ),
        None => (
            "Developer Mode 的注册表值**不存在** —— 按未开启处理（未提权时 symlink 会失败）"
                .to_owned(),
            vec!["developer-mode=absent".to_owned()],
        ),
    };
    out.push(Finding::new(
        ids::SYSTEM_DEVELOPER_MODE,
        Severity::Info,
        message,
        evidence,
        sources::REGISTRY,
    ));
}

/// `LongPathsEnabled`。`info`。
///
/// 为什么值得报：它决定**相对路径能不能超过 260 字符**。注意它管不了
/// `PATH` 条目本身 —— 相对路径永远受 `MAX_PATH` 限制（`\\?\` 无法加前缀）。
fn long_paths(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let (message, evidence) = match facts.system.long_paths {
        Some(true) => (
            "长路径**已开启**（`LongPathsEnabled=1`）—— 相对路径可以超过 260 字符".to_owned(),
            vec!["long-paths=true".to_owned()],
        ),
        Some(false) => (
            "长路径**未开启** —— 路径超过 260 字符时会失败".to_owned(),
            vec!["long-paths=false".to_owned()],
        ),
        None => (
            "长路径的注册表值**不存在** —— 按未开启处理（路径超过 260 字符时会失败）".to_owned(),
            vec!["long-paths=absent".to_owned()],
        ),
    };
    out.push(Finding::new(
        ids::SYSTEM_LONG_PATHS,
        Severity::Info,
        message,
        evidence,
        sources::REGISTRY,
    ));
}

/// WSL 发行版的 vhdx 不在默认位置。
///
/// 为什么值得报：换机器时**它不会被跟着搬** —— 默认位置的发行版跟着
/// `%LOCALAPPDATA%` 走，而放在 `C:\linux\...` 的那种要单独搬，
/// 搬错了就是"整台机器看着都在，只有这个发行版打不开"。
///
/// **标准位置的发行版不报**（本机 `docker-desktop` 就是标准位置的反例）。
/// 判据是 `non_standard_path`，而它已经处理过 `\\?\` 前缀（真机上
/// docker-desktop 的 `BasePath` 就是 `\\?\C:\Users\…` —— 不剥前缀会把
/// 默认位置的发行版误报成非标准）。
fn wsl_nonstandard_path(facts: &MachineFacts, out: &mut Vec<Finding>) {
    for distribution in facts
        .wsl
        .distribution
        .iter()
        .filter(|row| row.non_standard_path)
    {
        let mut message = format!(
            "WSL 发行版 `{}` 的 vhdx 不在默认位置（`{}`）—— 换机器时它不会被跟着搬，\
             要单独搬；搬错了就是\"整台机器看着都在，只有这个发行版打不开\"",
            distribution.name, distribution.base_path
        );
        if distribution.vhdx_exists == crate::capture::Existence::No {
            message.push_str("。**而且它的 vhdx 现在就不在**（这条比位置更要紧）");
        }

        out.push(Finding::new(
            ids::SYSTEM_WSL_NONSTANDARD_PATH,
            Severity::Info,
            message,
            vec![
                format!("distribution={}", distribution.name),
                format!("base-path={}", distribution.base_path),
                format!("vhdx-path={}", distribution.vhdx_path),
                format!("vhdx-exists={}", distribution.vhdx_exists.as_str()),
                "non-standard-path=true".to_owned(),
            ],
            sources::WSL,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::Existence;
    use crate::capture::files::{EnvFile, PathBudgetRow, PathFile, ToolsFile, WslFile, WslRow};
    use crate::doctor::facts::{self, ShimFacts};

    fn machine(wsl: WslFile) -> MachineFacts {
        MachineFacts {
            path: PathFile::new(
                "",
                PathBudgetRow {
                    raw_user_chars: 0,
                    raw_machine_chars: 0,
                    effective_chars: 100,
                    cliff: 8191,
                    remaining: 8091,
                    level: "ok".to_owned(),
                },
            ),
            env: EnvFile::new(""),
            tools: ToolsFile::new(""),
            wsl,
            system: facts::SystemFacts::default(),
            resolution: Vec::new(),
            global_prefix: Vec::new(),
            dev_roots: Vec::new(),
            shims: ShimFacts::default(),
        }
    }

    fn with_system(
        elevated: Option<bool>,
        developer_mode: Option<bool>,
        long_paths: Option<bool>,
    ) -> MachineFacts {
        let mut facts = machine(WslFile::new(""));
        facts.system = facts::SystemFacts {
            elevated,
            developer_mode,
            long_paths,
        };
        facts
    }

    fn ids_of(findings: &[Finding]) -> Vec<&'static str> {
        findings.iter().map(|finding| finding.id).collect()
    }

    fn find<'a>(findings: &'a [Finding], id: &str) -> &'a Finding {
        findings
            .iter()
            .find(|finding| finding.id == id)
            .unwrap_or_else(|| panic!("没有 {id}：{:?}", ids_of(findings)))
    }

    // ── system.elevated ─────────────────────────────────────────────

    #[test]
    fn elevated_is_reported_three_ways_and_unknown_is_not_false() {
        for (value, expected) in [
            (Some(true), "elevated=true"),
            (Some(false), "elevated=false"),
            (None, "elevated=unknown"),
        ] {
            let findings = run(&with_system(value, None, None));
            let finding = find(&findings, ids::SYSTEM_ELEVATED);
            assert_eq!(finding.severity, Severity::Info);
            assert_eq!(finding.evidence, vec![expected.to_owned()], "{value:?}");
        }
        // `None` 的措辞必须是"不知道"，不能是"未提权"。
        let unknown = run(&with_system(None, None, None));
        assert!(
            find(&unknown, ids::SYSTEM_ELEVATED)
                .message
                .contains("问不出来")
        );
        assert!(
            !find(&unknown, ids::SYSTEM_ELEVATED)
                .message
                .contains("没有**管理员权限")
        );
    }

    // ── system.developer-mode ───────────────────────────────────────

    #[test]
    fn developer_mode_distinguishes_on_off_and_absent() {
        for (value, expected) in [
            (Some(true), "developer-mode=true"),
            (Some(false), "developer-mode=false"),
            (None, "developer-mode=absent"),
        ] {
            let findings = run(&with_system(None, value, None));
            assert_eq!(
                find(&findings, ids::SYSTEM_DEVELOPER_MODE).evidence,
                vec![expected.to_owned()],
                "{value:?}"
            );
        }
        // "键不在"与"关着"要分开说 —— 但两者的**后果**相同（symlink 会失败），
        // 所以两条消息都要说清后果。
        let absent = run(&with_system(None, None, None));
        let message = &find(&absent, ids::SYSTEM_DEVELOPER_MODE).message;
        assert!(message.contains("不存在"), "{message}");
        assert!(message.contains("symlink 会失败"), "{message}");
    }

    // ── system.long-paths ───────────────────────────────────────────

    #[test]
    fn long_paths_distinguishes_on_off_and_absent() {
        for (value, expected) in [
            (Some(true), "long-paths=true"),
            (Some(false), "long-paths=false"),
            (None, "long-paths=absent"),
        ] {
            let findings = run(&with_system(None, None, value));
            assert_eq!(
                find(&findings, ids::SYSTEM_LONG_PATHS).evidence,
                vec![expected.to_owned()],
                "{value:?}"
            );
        }
    }

    // ── system.wsl-nonstandard-path ─────────────────────────────────

    fn distribution(name: &str, base: &str, non_standard: bool) -> WslRow {
        WslRow {
            name: name.to_owned(),
            guid: "{00000000-0000-0000-0000-000000000000}".to_owned(),
            base_path: base.to_owned(),
            non_standard_path: non_standard,
            wsl_version: Some(2),
            state: Some(1),
            vhdx_path: format!(r"{base}\ext4.vhdx"),
            vhdx_exists: Existence::Yes,
            vhdx_bytes: Some(1548746752),
        }
    }

    #[test]
    fn a_non_standard_distribution_is_reported_per_distribution() {
        let mut wsl = WslFile::new("");
        wsl.distribution.push(distribution(
            "Arch-Linux-current",
            r"C:\linux\Arch-Linux-current",
            true,
        ));
        wsl.distribution.push(distribution(
            "Ubuntu",
            r"C:\Users\me\AppData\Local\WSL\Ubuntu",
            true,
        ));
        // 反例：标准位置的那一个不报（本机 docker-desktop 就是这样）。
        wsl.distribution.push(distribution(
            "docker-desktop",
            r"C:\Users\me\AppData\Local\Docker\wsl\main",
            false,
        ));

        let findings = run(&machine(wsl));
        let reported: Vec<&Finding> = findings
            .iter()
            .filter(|finding| finding.id == ids::SYSTEM_WSL_NONSTANDARD_PATH)
            .collect();
        assert_eq!(reported.len(), 2, "每个非标准位置的发行版一条");
        assert!(reported.iter().all(|f| f.severity == Severity::Info));
        assert!(
            !reported
                .iter()
                .any(|f| f.message.contains("docker-desktop")),
            "标准位置的发行版不许被报"
        );
        assert!(reported.iter().any(|f| {
            f.evidence
                .contains(&r"base-path=C:\linux\Arch-Linux-current".to_owned())
        }));
    }

    #[test]
    fn a_missing_vhdx_makes_the_message_say_so() {
        let mut wsl = WslFile::new("");
        let mut row = distribution("Gone", r"C:\linux\Gone", true);
        row.vhdx_exists = Existence::No;
        wsl.distribution.push(row);
        let findings = run(&machine(wsl));
        assert!(
            find(&findings, ids::SYSTEM_WSL_NONSTANDARD_PATH)
                .message
                .contains("vhdx 现在就不在")
        );
    }

    #[test]
    fn a_machine_with_only_standard_distributions_reports_no_wsl_finding() {
        let mut wsl = WslFile::new("");
        wsl.distribution.push(distribution(
            "docker-desktop",
            r"C:\Users\me\AppData\Local\Docker\wsl\main",
            false,
        ));
        let findings = run(&machine(wsl));
        assert!(
            !ids_of(&findings).contains(&ids::SYSTEM_WSL_NONSTANDARD_PATH),
            "标准位置 = 这条检查的反例"
        );
    }

    // ── 全局 ────────────────────────────────────────────────────────

    #[test]
    fn the_three_status_checks_always_report_exactly_one_each() {
        // 这一族的形状：三条状态报告永远各有一条（票据把它们的严重度定成 info，
        // 就是这个意思），而它们**不是**噪声 —— 没有它们，读报告的人无法解释
        // "为什么 junction 能建而 symlink 不能"。
        let findings = run(&with_system(Some(false), None, Some(true)));
        assert_eq!(
            ids_of(&findings),
            vec![
                ids::SYSTEM_ELEVATED,
                ids::SYSTEM_DEVELOPER_MODE,
                ids::SYSTEM_LONG_PATHS
            ]
        );
        assert!(findings.iter().all(|f| f.severity == Severity::Info));
    }

    #[test]
    fn every_finding_carries_ascii_evidence_because_json_must_stay_parseable() {
        // `evidence` 是数据、`message` 是散文 —— 这条断言钉住那条分界。
        let mut wsl = WslFile::new("");
        wsl.distribution
            .push(distribution("Arch", r"C:\linux\Arch", true));
        let findings = run(&machine(wsl));
        for finding in &findings {
            for line in &finding.evidence {
                assert!(
                    line.is_ascii(),
                    "evidence 里出现了非 ASCII：{line}（中文该在 message 里）"
                );
            }
        }
    }
}
