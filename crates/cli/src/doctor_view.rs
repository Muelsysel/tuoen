//! `tuoen doctor` 的 **`--json` 形状**与**人类输出**。
//!
//! 与 `capture_cmd` 的做法一致：命令族一个 `*_view.rs`，视图与打印都在里面，
//! 编排在 [`crate::doctor_cmd`]。
//!
//! # 两条硬规矩
//!
//! 1. **`--json` 的成功载荷里不出现中文**（决策 35）。所以结论性的字段一律是
//!    **稳定 slug**：`severity` 是 `error` / `warn` / `info`（[`Severity::as_str`]），
//!    `source` 是 [`tuoen_core::doctor::sources`] 里的那几个，`id` 是
//!    [`tuoen_core::doctor::ids`] 里那 23 个。中文只在**人类输出**里。
//! 2. **`message` 根本不进 `--json`** —— 它在这份视图里**没有对应的字段**，
//!    而不是被序列化成空串或被 `skip` 掉。这不是省事：`Finding::message` 天生是
//!    一句中文（"机器级 PATH 第 21 段是空的"），而机器要的判据是 `id` 那个 slug
//!    （脚本按 `id` 分支，`message` 随界面语言与措辞改动而变）。
//!    两条出口各取所需：**人**读 [`print_human`]，**脚本**读 [`DoctorView`]。
//!
//! # 为什么 `evidence` 可以进 JSON（而 `message` 不行）
//!
//! [`tuoen_core::doctor::Finding::evidence`] 的契约是"位置/名字 + 值"的数据
//! （`machine#21`、`C:\Program Files\…`、`tool=java command=javac dir=…`），
//! 不是散文。数据能被脚本按字面匹配，散文不能 —— 而"到底是哪几条"正是这份
//! 报告的全部价值，去掉它 JSON 就只剩一堆计数。含非 ASCII 的路径由 `serde_json`
//! 转义成 `\uXXXX`，所以输出永远是纯 ASCII，逐字节稳定这一条不受影响。
//!
//! # 为什么 `counts` 与 `summary` 不直接序列化 `tuoen_core` 的那两个结构体
//!
//! 它们的字段名是 **Rust 的**（`path_entries`），而 `--json` 的键名是**对外契约**
//! （`pathEntries`）。让 TOML/Rust 的命名变成事实上的公开 API，正是 `main.rs`
//! 的模块文档点名要避免的事（"`--json` 的载荷全部来自 `view` 模块"）。
//! 于是键名与驼峰拼法都只有这一处定义，改它就是破坏性变更（要递增信封的
//! `schemaVersion`）。

use serde::Serialize;
use tuoen_core::doctor::{DoctorReport, Finding, Severity};

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 形状
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen doctor --json` 的成功载荷。
#[derive(Debug, Serialize)]
pub struct DoctorView {
    /// 各严重度的条数。**脚本要的判据就是 `counts.error`** —— 它比退出码准：
    /// 退出码 0 说的是"体检跑完了"，而"发现了几个要动手的问题"是这个数字。
    pub counts: CountsView,
    /// 这一次**看了多少**（分母）。少一条发现时，读的人要能分辨
    /// "没有"与"没看" —— 那份区分就靠这几个数字。
    pub summary: SummaryView,
    /// 按（严重度，ID）排好序的发现。顺序由 `tuoen_core::doctor::diagnose` 定，
    /// 这一层**不重排**：重排会让"两次跑逐字节相同"变成两个地方的责任。
    pub findings: Vec<FindingView>,
}

impl DoctorView {
    /// 从体检结果组装。**每一个数字都从 [`DoctorReport`] 里取**，
    /// 这一层不数第二遍 —— 数两遍就会出现"报告里的计数与明细对不上"。
    #[must_use]
    pub fn new(report: &DoctorReport) -> Self {
        Self {
            counts: CountsView::from(report),
            summary: SummaryView::from(report),
            findings: report.findings.iter().map(FindingView::from).collect(),
        }
    }
}

/// 各严重度的条数。键名就是 `Severity` 的三个 slug（不本地化）。
#[derive(Debug, Serialize)]
pub struct CountsView {
    pub error: usize,
    pub warn: usize,
    pub info: usize,
}

impl From<&DoctorReport> for CountsView {
    fn from(report: &DoctorReport) -> Self {
        Self {
            error: report.counts.error,
            warn: report.counts.warn,
            info: report.counts.info,
        }
    }
}

/// 事实的规模。**分母必须说出来**：`path.shadowed` 报 0 条时，读的人要知道
/// 那是因为"没有 shim"（`shimsOnDisk` 是 0）还是"有 shim 但没被抢"。
#[derive(Debug, Serialize)]
pub struct SummaryView {
    /// `PATH` 条目数（三个作用域）。
    #[serde(rename = "pathEntries")]
    pub path_entries: usize,
    /// 持久环境变量数（用户级 + 机器级）。
    #[serde(rename = "envVars")]
    pub env_vars: usize,
    /// 检测到的工具行数。
    #[serde(rename = "toolRows")]
    pub tool_rows: usize,
    /// WSL 发行版数。
    #[serde(rename = "wslDistributions")]
    pub wsl_distributions: usize,
    /// 盘上真实的 shim 数。
    #[serde(rename = "shimsOnDisk")]
    pub shims_on_disk: usize,
    /// 我们解析过的命令数。
    #[serde(rename = "resolvedCommands")]
    pub resolved_commands: usize,
}

impl From<&DoctorReport> for SummaryView {
    fn from(report: &DoctorReport) -> Self {
        let summary = &report.summary;
        Self {
            path_entries: summary.path_entries,
            env_vars: summary.env_vars,
            tool_rows: summary.tool_rows,
            wsl_distributions: summary.wsl_distributions,
            shims_on_disk: summary.shims_on_disk,
            resolved_commands: summary.resolved_commands,
        }
    }
}

/// 一条发现的机器可读形状。**没有 `message` 字段** —— 理由见模块文档。
#[derive(Debug, Serialize)]
pub struct FindingView {
    /// 稳定 ID，取自 [`tuoen_core::doctor::ids`]。
    pub id: &'static str,
    /// 稳定 slug：`error` / `warn` / `info`（**不是**人类输出里的"错误/警告/提示"）。
    pub severity: &'static str,
    /// 具体是哪几条（数据，不是散文；可能是路径）。
    pub evidence: Vec<String>,
    /// 这条结论从哪来，取自 [`tuoen_core::doctor::sources`]。
    pub source: &'static str,
    /// 与检测引擎同源的置信度。**只有真的有值时才出现**：
    /// 一个恒为 `null` 的键会让消费者以为"这一条问了但没答出来"，
    /// 而实际是"这一条根本不来自检测引擎"（见 `Finding::confidence` 的文档）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<&'static str>,
}

impl From<&Finding> for FindingView {
    fn from(finding: &Finding) -> Self {
        Self {
            id: finding.id,
            severity: finding.severity.as_str(),
            evidence: finding.evidence.clone(),
            source: finding.source,
            confidence: finding.confidence,
        }
    }
}

/// 装配失败时的载荷：**只带我们确实知道的那两件事**。
///
/// 它刻意**没有** `counts` / `summary` / `findings` —— 体检根本没跑，
/// 报一个空数组或三个 0 就是在说"我看过了，什么都没发现"。
/// "缺席 = 我们不知道"比"空数组 = 我们知道是零"诚实（与 `capture` 失败路径
/// 里"不给 `files`"是同一条规矩）。
#[derive(Debug, Serialize)]
pub struct UnwiredRootsView {
    /// 算出来的存储根。**相对路径就是失败的原因本身**，所以原样报出来。
    #[serde(rename = "storeRoot")]
    pub store_root: String,
    /// 算出来的 shim 目录。
    #[serde(rename = "shimDir")]
    pub shim_dir: String,
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen doctor` 的人类输出。
///
/// 三段：按严重度分组的发现（错误 → 警告 → 提示）、一句"这次只读"、
/// 以及**最后两行**（汇总 + 规模）。
///
/// 为什么把汇总与规模放在最末尾：读一份体检报告的动作是"从上往下看到底"，
/// 而最需要被看见的两个结论是"有几个要动手的"与"这一次看了多少"。放在开头
/// 会被发现列表顶走，放在结尾正好落在读完的地方。
///
/// 为什么 `message` 与 `evidence` 都印：`message` 说"这是什么问题"，
/// `evidence` 说"具体是哪几条" —— 只有前者时用户不知道从哪下手，
/// 只有后者时用户不知道为什么要下手。
pub fn print_human(report: &DoctorReport, no_probe: bool) {
    println!("tuoen doctor —— 环境体检（**只报告，不修改**）。");
    println!();

    if report.findings.is_empty() {
        // **"没有问题"必须说出来，而且要连着规模一起说。** 只印规模会让读的人
        // 自己去做减法（"这是没有发现，还是没看？"）；只印"没有问题"更糟 ——
        // 那是本仓库最不能出现的那类话："一句看起来完全合理的错话"。
        println!("没有发现问题。");
        println!(
            "（这句话要和最后那行规模一起读 —— 它说的是「看了这些之后没有问题」，\
             不是「这台机器是健康的」。）"
        );
        println!();
    } else {
        for severity in [Severity::Error, Severity::Warn, Severity::Info] {
            let group: Vec<&Finding> = report
                .findings
                .iter()
                .filter(|finding| finding.severity == severity)
                .collect();
            if group.is_empty() {
                continue;
            }
            println!("{} {} 条：", severity.label(), group.len());
            for finding in group {
                println!("  · {}", finding.id);
                println!("    {}", finding.message);
                // 空 `evidence` 的条目**不印一个空的列表头** —— 一个下面什么都没有的
                // "证据："看起来像输出被截断了。
                if !finding.evidence.is_empty() {
                    println!("    证据：");
                    for item in &finding.evidence {
                        println!("      · {item}");
                    }
                }
            }
            println!();
        }
    }

    if no_probe {
        println!(
            "（没有跑「全局包前缀在哪」那条探测 —— 加了 `--no-probe`。\
             它要启动 `npm` 这类工具问一句，所以慢，但它也是 `tool.global-prefix-inside-version-dir`\
             唯一的输入：这一条不跑，那一类结论就是「没看」。）"
        );
    }
    println!("（本次只读：没有写 `PATH`、没有写注册表、没有动任何工具。）");

    if report.counts.has_error() {
        // **退出码 0 在这里最容易被误读成"一切正常"。** 误会发生在读报告的这一刻，
        // 所以解释也要印在这一刻，而不是只写在 `--help` 里。
        println!(
            "（上面有 {} 条是「要动手」的，但这次体检本身跑完了 —— \
             所以退出码是 0。脚本要判据请用 `--json` 里的 `counts.error`。）",
            report.counts.error
        );
    }
    println!();

    let counts = &report.counts;
    println!(
        "错误 {} · 警告 {} · 提示 {}",
        counts.error, counts.warn, counts.info
    );
    let summary = &report.summary;
    println!(
        "规模：PATH 条目 {} · 环境变量 {} · 工具 {} · WSL 发行版 {} · shim {}",
        summary.path_entries,
        summary.env_vars,
        summary.tool_rows,
        summary.wsl_distributions,
        summary.shims_on_disk
    );
}

#[cfg(test)]
mod tests {
    use tuoen_core::doctor::{Counts, FactsSummary, Finding, ids, sources};

    use super::*;

    fn has_cjk(text: &str) -> bool {
        text.chars()
            .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
    }

    fn a_report() -> DoctorReport {
        DoctorReport {
            findings: vec![
                Finding::new(
                    ids::PATH_LENGTH_BUDGET,
                    Severity::Error,
                    "生效 `PATH` 逼近 cmd.exe 的悬崖",
                    vec!["effective=8100 cliff=8191".to_owned()],
                    sources::PATH_RESOLUTION,
                ),
                Finding::new(
                    ids::TOOL_GHOST,
                    Severity::Warn,
                    "注册表声称已安装，但文件不存在",
                    Vec::new(),
                    sources::REGISTRY,
                )
                .with_confidence("registered-missing"),
            ],
            counts: Counts {
                error: 1,
                warn: 1,
                info: 0,
            },
            summary: FactsSummary {
                path_entries: 41,
                env_vars: 68,
                tool_rows: 17,
                wsl_distributions: 0,
                shims_on_disk: 3,
                resolved_commands: 33,
            },
        }
    }

    #[test]
    fn a_finding_serialises_without_its_message_and_keeps_the_confidence_only_when_it_has_one() {
        let report = a_report();
        let view = DoctorView::new(&report);
        let json = serde_json::to_string(&view).expect("序列化");
        assert!(
            !has_cjk(&json),
            "`--json` 的成功载荷里不该有中文（决策 35）：{json}"
        );
        assert!(
            !json.contains("message"),
            "`message` 天生是中文，不许出现在成功载荷里：{json}"
        );
        // 有置信度的那一条带着它，没有的那一条**连键都不出现**。
        assert!(
            json.contains(r#""confidence":"registered-missing""#),
            "{json}"
        );
        assert_eq!(
            json.matches("\"confidence\"").count(),
            1,
            "只有真的有置信度时才留这个键：{json}"
        );
    }

    #[test]
    fn the_json_keys_are_the_public_contract_not_the_rust_field_names() {
        let report = a_report();
        let view = DoctorView::new(&report);
        let json = serde_json::to_string(&view).expect("序列化");
        for key in [
            r#""counts":{"error":1,"warn":1,"info":0}"#,
            r#""pathEntries":41"#,
            r#""envVars":68"#,
            r#""toolRows":17"#,
            r#""wslDistributions":0"#,
            r#""shimsOnDisk":3"#,
            r#""resolvedCommands":33"#,
            r#""severity":"error""#,
            r#""source":"path-resolution""#,
        ] {
            assert!(json.contains(key), "缺少 `{key}`：{json}");
        }
        // 蛇形键名是 Rust 的字段名，不该泄漏成对外契约。
        assert!(!json.contains("path_entries"), "{json}");
        assert!(!json.contains("shims_on_disk"), "{json}");
    }

    #[test]
    fn the_counts_come_from_the_report_and_the_findings_keep_their_order() {
        let report = a_report();
        let view = DoctorView::new(&report);
        assert_eq!(view.counts.error, 1);
        assert_eq!(view.counts.warn, 1);
        assert_eq!(view.counts.info, 0);
        let ids_in_view: Vec<&str> = view.findings.iter().map(|f| f.id).collect();
        let ids_in_report: Vec<&str> = report.findings.iter().map(|f| f.id).collect();
        assert_eq!(ids_in_view, ids_in_report, "视图不重排发现");
    }

    #[test]
    fn the_failure_payload_carries_the_roots_and_no_fake_counts() {
        let view = UnwiredRootsView {
            store_root: r".tuoen\store".to_owned(),
            shim_dir: r".tuoen\shims".to_owned(),
        };
        let json = serde_json::to_string(&view).expect("序列化");
        assert_eq!(
            json,
            r#"{"storeRoot":".tuoen\\store","shimDir":".tuoen\\shims"}"#
        );
        assert!(!json.contains("counts"), "没跑过的体检不许报计数：{json}");
        assert!(!json.contains("findings"), "{json}");
    }
}
