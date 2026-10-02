//! `tuoen install` / `use` / `uninstall` 的 **`--json` 呈现层**。
//!
//! 与 [`crate::view`] 分开是有理由的，不是为了分文件：那一族描述的是**目录里有什么**
//! （静态的、可缓存的），这一族描述的是**刚刚发生了什么**（一次运行的结果，
//! 含耗时、走过哪些源、哪一步拒绝了你）。两者的消费者不同，混在一起会让
//! 某一族需要的字段污染另一族。
//!
//! 键名 camelCase、取值是稳定的机器可读字符串 —— 与既有契约一致（决策 35）。

use serde::Serialize;
use tuoen_archive::AuditReport;
use tuoen_download::{Attempt, DownloadError, FailureKind, FetchReport};
use tuoen_manifest::{Layout, ResolvedRecipe};
use tuoen_platform::Repoint;
use tuoen_store::{AdoptOutcome, UninstallOutcome};

/// `tuoen install --json` 的载荷。
///
/// **`--dry-run` 与真装用的是同一个形状**，区别只有一个：`result` 为 `null`。
/// 为什么不给 `--dry-run` 单独一套形状 —— 那样消费者要写两套解析，
/// 而"我只想看会做什么"和"我真做了"要读的字段其实是同一批（装到哪、从哪下、什么哈希）。
#[derive(Debug, Serialize)]
pub struct InstallView {
    #[serde(rename = "dryRun")]
    pub dry_run: bool,
    /// **计划**：无论 dry-run 还是真装都一定有。
    pub plan: InstallPlanView,
    /// **结果**：`--dry-run` 时是 `null`（什么都没发生）。
    pub result: Option<InstallResultView>,
}

/// 这次安装打算做什么。全部来自内置目录，**不需要网络**。
#[derive(Debug, Serialize)]
pub struct InstallPlanView {
    pub tool: String,
    pub version: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub platform: String,
    /// 生效的许可证结论（`allowed` / `metadata-only` / `conditional`）。
    /// **`prohibited` 走不到这里** —— 门禁在解析 recipe 之前就拒了。
    pub redistribution: &'static str,
    #[serde(rename = "licenceName")]
    pub licence_name: String,
    pub url: String,
    pub sha256: String,
    pub archive: &'static str,
    /// 载荷最终落在哪（版本目录）。
    #[serde(rename = "installedTo")]
    pub installed_to: String,
    /// 激活时会翻转的那个链接。
    #[serde(rename = "currentLink")]
    pub current_link: String,
    /// 解压后的布局：剥层数、要暴露的命令、环境根。
    pub layout: LayoutView,
    /// 这个版本**现在**是不是已经装过了。
    #[serde(rename = "alreadyInstalled")]
    pub already_installed: bool,
    /// 这次是不是要顺手激活（`--use`）。
    #[serde(rename = "activateAfter")]
    pub activate_after: bool,
    /// 装完之后该敲的那条命令。`activate_after` 为真时是 `null`。
    #[serde(rename = "nextCommand")]
    pub next_command: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LayoutView {
    #[serde(rename = "stripComponents")]
    pub strip_components: u32,
    /// 命令名 → 归档内的相对路径。**这是"装完能敲什么"的权威答案。**
    pub bin: std::collections::BTreeMap<String, String>,
    /// 环境根（JDK 的 `.` 就是版本目录本身），没有就是 `null`。
    pub home: Option<String>,
}

impl From<&Layout> for LayoutView {
    fn from(layout: &Layout) -> Self {
        Self {
            strip_components: layout.strip_components,
            bin: layout.bin.clone(),
            home: layout.home.clone(),
        }
    }
}

impl InstallPlanView {
    /// 从已解析的 recipe + 目录里的实际位置构造。
    #[must_use]
    pub fn new(
        recipe: &ResolvedRecipe,
        installed_to: String,
        current_link: String,
        already_installed: bool,
        activate_after: bool,
    ) -> Self {
        Self {
            tool: recipe.tool_id.clone(),
            version: recipe.version.clone(),
            display_name: recipe.display_name.clone(),
            platform: recipe.platform.clone(),
            redistribution: recipe.redistribution.as_str(),
            licence_name: recipe.licence_name.clone(),
            url: recipe.url.clone(),
            sha256: recipe.checksum.value.clone(),
            archive: recipe.archive.as_str(),
            installed_to,
            current_link,
            layout: LayoutView::from(&recipe.layout),
            already_installed,
            activate_after,
            next_command: if activate_after {
                None
            } else {
                Some(format!("tuoen use {} {}", recipe.tool_id, recipe.version))
            },
        }
    }
}

/// 真的做了什么。
#[derive(Debug, Serialize)]
pub struct InstallResultView {
    /// 实际服务了这次下载的源 id（`cn-npmmirror` / `official` / 用户模板名）。
    pub source: String,
    /// 那个源上的最终 URL（可能经过了重定向）。
    pub url: String,
    /// 归档在缓存里的路径。
    #[serde(rename = "cachedPath")]
    pub cached_path: String,
    pub bytes: u64,
    #[serde(rename = "fromCache")]
    pub from_cache: bool,
    /// 走到成功之前失败过几次（0 表示第一个源就成了）。
    #[serde(rename = "failuresBeforeSuccess")]
    pub failures_before_success: usize,
    /// 走过的每一个源，按顺序。
    pub attempts: Vec<AttemptView>,
    /// 归档里有多少条目。
    #[serde(rename = "listedEntries")]
    pub listed_entries: usize,
    /// 落盘审计的结论 —— **这是"解压出来的东西真的是它声称的东西"的判据**。
    pub audit: AuditView,
    /// 载荷里有多少个文件、多少字节。
    pub files: u64,
    #[serde(rename = "payloadBytes")]
    pub payload_bytes: u64,
    /// 装好之后翻没翻 `current`。
    pub activated: bool,
    /// 翻转的结果（`activated` 为假时是 `null`）。
    pub repoint: Option<RepointView>,
}

impl InstallResultView {
    #[must_use]
    pub fn new(
        report: &FetchReport,
        listed_entries: usize,
        audit: &AuditReport,
        adopted: &AdoptOutcome,
        repoint: Option<&Repoint>,
    ) -> Self {
        Self {
            source: report.source_id.clone(),
            url: report.source_url.clone(),
            cached_path: report.path.display().to_string(),
            bytes: report.bytes,
            from_cache: report.from_cache,
            failures_before_success: report.failures_before_success(),
            attempts: report.attempts.iter().map(AttemptView::from).collect(),
            listed_entries,
            audit: AuditView::from(audit),
            files: adopted.files,
            payload_bytes: adopted.bytes,
            activated: repoint.is_some(),
            repoint: repoint.map(RepointView::from),
        }
    }
}

/// 落盘审计的结论。
#[derive(Debug, Serialize)]
pub struct AuditView {
    pub entries: usize,
    pub bytes: u64,
    #[serde(rename = "maxDepth")]
    pub max_depth: usize,
}

impl From<&AuditReport> for AuditView {
    fn from(audit: &AuditReport) -> Self {
        Self {
            entries: audit.entries,
            bytes: audit.total_bytes,
            max_depth: audit.max_depth,
        }
    }
}

/// 一个源上的一次尝试。
#[derive(Debug, Serialize)]
pub struct AttemptView {
    pub source: String,
    pub url: String,
    pub ok: bool,
    /// **稳定的机器可读错误码**（`source-unavailable` / `integrity` / …）。
    /// 失败时才有；中文消息在 `message` 里，两者分开。
    pub code: Option<&'static str>,
    pub message: Option<String>,
    pub millis: u64,
}

impl From<&Attempt> for AttemptView {
    fn from(attempt: &Attempt) -> Self {
        Self {
            source: attempt.source_id.clone(),
            url: attempt.url.clone(),
            ok: attempt.ok,
            code: attempt.kind.map(failure_kind_code),
            message: attempt.error.clone(),
            millis: attempt.millis,
        }
    }
}

/// `FailureKind` → 稳定 slug。
///
/// **委托给 `FailureKind::as_str()`，不自己再写一张表。** 两处各写一遍
/// 就会出现"检测输出说 `source-unavailable`、install 输出说 `source_unavailable`"
/// 这种最难查的不一致 —— 而它已经有一个权威定义，就在那个类型上。
#[must_use]
pub const fn failure_kind_code(kind: FailureKind) -> &'static str {
    kind.as_str()
}

/// 下载/校验失败的稳定错误码。与 [`failure_kind_code`] 同一套取值。
#[must_use]
pub fn download_error_code(error: &DownloadError) -> &'static str {
    error.kind().as_str()
}

/// `Repoint` → 稳定字符串。
#[derive(Debug, Serialize)]
pub struct RepointView {
    /// `created` / `replaced` / `degraded` —— **不本地化**。
    pub outcome: &'static str,
    /// `degraded` 时说明为什么退化了（可能要删掉再建，那条路径**不是原子的**）。
    #[serde(rename = "degradedReason")]
    pub degraded_reason: Option<String>,
}

impl From<&Repoint> for RepointView {
    fn from(repoint: &Repoint) -> Self {
        match repoint {
            Repoint::Created => Self {
                outcome: "created",
                degraded_reason: None,
            },
            Repoint::Replaced => Self {
                outcome: "replaced",
                degraded_reason: None,
            },
            Repoint::Degraded { reason } => Self {
                outcome: "degraded",
                degraded_reason: Some(reason.clone()),
            },
        }
    }
}

/// `tuoen use --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct UseView {
    pub tool: String,
    pub version: String,
    /// 被翻转的那个链接。
    pub link: String,
    /// 它现在指向哪。
    pub target: String,
    /// 翻转之前指向哪个版本（第一次激活时是 `null`）。
    pub previous: Option<String>,
    pub repoint: RepointView,
}

/// `tuoen uninstall --json` 的载荷。
#[derive(Debug, Serialize)]
pub struct UninstallView {
    pub tool: String,
    pub version: String,
    /// 删掉的正好是当时生效的那个吗。
    #[serde(rename = "wasActive")]
    pub was_active: bool,
    #[serde(rename = "freedFiles")]
    pub freed_files: u64,
    #[serde(rename = "freedBytes")]
    pub freed_bytes: u64,
}

impl UninstallView {
    #[must_use]
    pub fn new(tool: &str, version: &str, outcome: &UninstallOutcome) -> Self {
        Self {
            tool: tool.to_owned(),
            version: version.to_owned(),
            was_active: outcome.was_active,
            freed_files: outcome.freed_files,
            freed_bytes: outcome.freed_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_manifest::ArchiveFormat;

    #[test]
    fn failure_kind_codes_are_stable_slugs_not_variant_names() {
        // 这些字符串是公开契约；改它们要递增 `schemaVersion`。
        assert_eq!(
            failure_kind_code(FailureKind::SourceUnavailable),
            "source-unavailable"
        );
        assert_eq!(failure_kind_code(FailureKind::Integrity), "integrity");
    }

    #[test]
    fn failure_codes_are_lowercase_kebab_ascii() {
        for kind in [
            FailureKind::SourceUnavailable,
            FailureKind::Integrity,
            FailureKind::LocalInput,
            FailureKind::LocalEnvironment,
        ] {
            let code = failure_kind_code(kind);
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c == '-' || c.is_ascii_digit()),
                "{code} 不是小写 kebab ASCII"
            );
            assert!(!code.is_empty());
        }
    }

    #[test]
    fn repoint_views_match_their_variants() {
        assert_eq!(
            RepointView::from(&Repoint::Created).outcome,
            "created",
            "第一次激活必须报 created"
        );
        assert_eq!(
            RepointView::from(&Repoint::Replaced).outcome,
            "replaced",
            "重指必须报 replaced —— 它是'原子翻转'的证据（决策 46）"
        );
        let degraded = RepointView::from(&Repoint::Degraded {
            reason: "原位置是符号链接".to_owned(),
        });
        assert_eq!(degraded.outcome, "degraded");
        assert_eq!(
            degraded.degraded_reason.as_deref(),
            Some("原位置是符号链接"),
            "退化必须带上原因 —— 否则用户不知道那条路径不是原子的"
        );
    }

    #[test]
    fn a_dry_run_plan_still_names_the_next_command() {
        // `--dry-run` 最有用的东西之一就是"那我接下来敲什么"。
        // 计划里 nextCommand 为 None 只在 activate_after 时发生。
        let layout = Layout::default();
        let recipe = ResolvedRecipe {
            tool_id: "node".to_owned(),
            display_name: "Node.js".to_owned(),
            version: "24.19.0".to_owned(),
            platform: "windows-x64".to_owned(),
            url: "https://example.invalid/node.zip".to_owned(),
            checksum: tuoen_manifest::Checksum {
                algorithm: "sha256".to_owned(),
                value: "a".repeat(64),
            },
            archive: ArchiveFormat::Zip,
            layout,
            redistribution: tuoen_manifest::Redistribution::Allowed,
            licence_name: "MIT".to_owned(),
            licence_notes: String::new(),
        };

        let plan = InstallPlanView::new(
            &recipe,
            r"C:\s\node\versions\24.19.0".to_owned(),
            r"C:\s\node\current".to_owned(),
            false,
            false,
        );
        assert_eq!(plan.next_command.as_deref(), Some("tuoen use node 24.19.0"));

        let plan = InstallPlanView::new(
            &recipe,
            r"C:\s\node\versions\24.19.0".to_owned(),
            r"C:\s\node\current".to_owned(),
            false,
            true,
        );
        assert_eq!(
            plan.next_command, None,
            "已经会激活了就不该再让用户敲一次 use"
        );
    }

    #[test]
    fn a_dry_run_install_view_serialises_with_a_null_result() {
        // 形状必须稳定：`--dry-run` 与真装是同一套键，只有 result 为 null。
        let view = InstallView {
            dry_run: true,
            plan: InstallPlanView {
                tool: "node".to_owned(),
                version: "24.19.0".to_owned(),
                display_name: "Node.js".to_owned(),
                platform: "windows-x64".to_owned(),
                redistribution: "allowed",
                licence_name: "MIT".to_owned(),
                url: "https://example.invalid/n.zip".to_owned(),
                sha256: "a".repeat(64),
                archive: "zip",
                installed_to: r"C:\s\v\24.19.0".to_owned(),
                current_link: r"C:\s\current".to_owned(),
                layout: LayoutView {
                    strip_components: 1,
                    bin: std::collections::BTreeMap::new(),
                    home: None,
                },
                already_installed: false,
                activate_after: false,
                next_command: Some("tuoen use node 24.19.0".to_owned()),
            },
            result: None,
        };
        let json = serde_json::to_string(&view).expect("serialise");
        assert!(json.contains(r#""dryRun":true"#), "{json}");
        assert!(json.contains(r#""result":null"#), "{json}");
        assert!(
            json.contains(r#""nextCommand":"tuoen use node 24.19.0""#),
            "{json}"
        );
        assert!(json.contains(r#""stripComponents":1"#), "{json}");
    }
}
