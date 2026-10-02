//! `path.*` 检查 —— `PATH` 的十条体检项。
//!
//! 这一族是票据 #13 里最重的一族，因为 `PATH` 是本项目里**唯一**一处
//! "一次调用就能让整台机器所有命令失效"的地方（`AGENTS.md` 的六条规矩）。
//!
//! # 一个 ID 一条发现，实例进 `evidence`
//!
//! 报告要能一眼看完，而 `evidence` 就是为"具体是哪几条"准备的。只有**每个实例的
//! 处置方式不同**时才拆成多条：这一族里只有 [`ids::PATH_SHADOWED`] 按**遮蔽目录**拆
//! （每个目录要用户做不同的事），其余九条各自只有一条发现，`evidence.len()`
//! 就是实例数。
//!
//! # 反例（健康机器上为什么不该响）
//!
//! - `path.duplicate` / `path.missing` / `path.username-hardcoded` / `path.empty-entry`
//!   / `path.relative` / `path.non-ascii` / `path.spaces`：没有任何一条条目命中判据。
//! - `path.length-budget`：**`ok` 档不产生任何发现** —— 一条永远响的检查等于噪声。
//! - `path.reparse`：全是普通目录（本机 46/49 条就是这样，响的那 3 条是真的）。
//! - `path.shadowed`：盘上没有 shim（分母为零 → 什么都不报，**不是**"没被遮蔽"）。

use std::collections::BTreeMap;

use tuoen_platform::{EnvScope, ReparseKind};

use super::super::{Finding, MachineFacts, Severity, facts, ids, sources};
use crate::capture::Existence;
use crate::capture::files::PathRow;

/// 跑这一族的十条检查。
#[must_use]
pub fn run(facts: &MachineFacts) -> Vec<Finding> {
    let mut out = Vec::new();
    duplicates(facts, &mut out);
    missing(facts, &mut out);
    username_hardcoded(facts, &mut out);
    shadowed(facts, &mut out);
    length_budget(facts, &mut out);
    empty_entry(facts, &mut out);
    reparse(facts, &mut out);
    relative(facts, &mut out);
    non_ascii(facts, &mut out);
    spaces(facts, &mut out);
    out
}

// ───────────────────────────── 十条检查 ─────────────────────────────

/// 重复条目。判据：**忽略大小写、去尾部反斜杠**后同值的条目出现两次以上。
///
/// 为什么是 `warn` 而不是 `error`：重复本身不会让命令失效（第一个命中的说了算），
/// 但它会把 `PATH` 推向 8191 悬崖，而且它让"到底哪个目录在生效"变得看不出来。
fn duplicates(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let mut groups: BTreeMap<String, Vec<&PathRow>> = BTreeMap::new();
    for row in &facts.path.entry {
        // 空条目不是"一个目录"，它没有"同值"可言（`;;` 之间没有可比较的值）。
        if row.empty {
            continue;
        }
        groups
            .entry(compare_key(&row.expanded))
            .or_default()
            .push(row);
    }

    let duplicated: Vec<(&String, &Vec<&PathRow>)> =
        groups.iter().filter(|(_, rows)| rows.len() > 1).collect();
    if duplicated.is_empty() {
        return;
    }

    // 富余条数 = 多出来的那些（一个值出现 3 次算 2 条富余）。
    // 它与"组数"是两个量，报告里要分开说 —— 票据里的"10 组"与真机的"18 条"
    // 就是这两个数（本机 18 条富余分布在 12 个组里）。
    let extra: usize = duplicated.iter().map(|(_, rows)| rows.len() - 1).sum();
    let evidence: Vec<String> = duplicated
        .iter()
        .map(|(key, rows)| {
            let where_: Vec<String> = rows.iter().map(|row| position(row)).collect();
            format!("{key} x{} ({})", rows.len(), where_.join(" "))
        })
        .collect();

    out.push(Finding::new(
        ids::PATH_DUPLICATE,
        Severity::Warn,
        format!(
            "`PATH` 上有 {} 组重复条目（{} 条富余）。重复本身不会让命令失效，\
             但它把 `PATH` 往 8191 的悬崖上推，而且让\"到底哪个目录在生效\"变得看不出来",
            duplicated.len(),
            extra
        ),
        evidence,
        sources::PATH_RESOLUTION,
    ));
}

/// 指向不存在路径的条目。
///
/// **含 `%` 的条目不在此列**：我们没展开它（`REG_EXPAND_SZ` 的展开是不可逆的
/// 信息损失），答不了的题不许猜 —— 那种条目的 `exists` 是 `unknown`。
/// 空条目的 `exists` 也是 `unknown`（决策 91）。
fn missing(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts
        .path
        .entry
        .iter()
        .filter(|row| !row.empty && row.exists == Existence::No)
        .collect();
    if rows.is_empty() {
        return;
    }

    let unknown = facts
        .path
        .entry
        .iter()
        .filter(|row| row.exists == Existence::Unknown && !row.empty)
        .count();
    let mut message = format!("`PATH` 上有 {} 条条目指向不存在的路径", rows.len());
    if unknown > 0 {
        message.push_str(&format!(
            "（另有 {unknown} 条含 `%VAR%` 的条目**没有判** —— 我们没展开它们，答不了的题不许猜）"
        ));
    }

    out.push(Finding::new(
        ids::PATH_MISSING,
        Severity::Warn,
        message,
        rows.iter()
            .map(|row| format!("{} {}", position(row), row.expanded))
            .collect(),
        sources::FILESYSTEM,
    ));
}

/// 硬编码了 `C:\Users\<某个名字>\` 的条目。**这是 `error`**。
///
/// 为什么是 `error`：它在**另一台机器上必然失效**，而且失效得很难看 ——
/// 用户名不同的机器上那条路径指向别人的目录（或者干脆不存在）。这是本项目
/// "整机开发状态可搬运"这块招牌的直接反面。
///
/// 机器级的那几条更重：用户级还能自己改，机器级要管理员权限。
fn username_hardcoded(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts
        .path
        .entry
        .iter()
        .filter(|row| row.has_username)
        .collect();
    if rows.is_empty() {
        return;
    }

    let machine_scope = rows
        .iter()
        .filter(|row| row.scope == EnvScope::Machine)
        .count();
    let mut message = format!(
        "`PATH` 上有 {} 条条目硬编码了 `C:\\Users\\<名字>\\`，换一台机器必然失效",
        rows.len()
    );
    if machine_scope > 0 {
        message.push_str(&format!(
            "；其中 {machine_scope} 条在**机器级**（改它们要管理员权限）"
        ));
    }
    message.push_str("。可搬运的写法是用 `%USERPROFILE%` 或 `%LOCALAPPDATA%`");

    let evidence: Vec<String> = rows
        .iter()
        .map(|row| {
            let scope = if row.scope == EnvScope::Machine {
                " (machine-scope)"
            } else {
                ""
            };
            format!("{} {}{scope}", position(row), row.expanded)
        })
        .collect();

    out.push(Finding::new(
        ids::PATH_USERNAME_HARDCODED,
        Severity::Error,
        message,
        evidence,
        sources::PATH_RESOLUTION,
    ));
}

/// 我们的 shim 被别的目录抢在前面。
///
/// # 按**遮蔽目录**拆成多条
///
/// 每一条要用户做的事不同（去掉哪一个目录、或者把 shim 目录往前挪），
/// 所以这里不合并成一条。
///
/// # 分母
///
/// 盘上一条 shim 都没有时**什么都不报** —— 那不是"没被遮蔽"，而是"没得比"
/// （决策 75 的教训：`shadowed = 0` 必须能区分这两种情况）。消息里会把分母说出来。
/// shim 目录不在 `PATH` 上时，消息里要**先说这件事**：那才是根因，
/// 而"被 X 遮蔽"只是它的后果。
fn shadowed(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let shims = &facts.shims;
    if shims.commands.is_empty() || shims.shadowed.is_empty() {
        return;
    }

    // 按遮蔽目录分组：谁抢的、抢了哪几条。
    let mut by_directory: BTreeMap<&str, Vec<&facts::ShadowFact>> = BTreeMap::new();
    for shim in &shims.shadowed {
        by_directory.entry(shim.by.as_str()).or_default().push(shim);
    }

    let shim_dir = shims.dir.as_deref().unwrap_or("<unknown>");
    for (directory, stolen) in by_directory {
        let commands: Vec<&str> = stolen.iter().map(|shim| shim.command.as_str()).collect();
        let mut message = format!(
            "我们发布的 {} 条 shim 里有 {} 条被 `{directory}` 抢在前面（一共 {} 条）",
            shims.commands.len(),
            stolen.len(),
            commands.join(" / ")
        );
        if shims.on_path {
            message.push_str("。要它们生效，得让 shim 目录排在这个目录前面");
        } else {
            message.push_str(&format!(
                "。**根因是 shim 目录 `{shim_dir}` 根本不在 `PATH` 上** —— \
                 先把它加进去，否则这些 shim 永远不会被用到"
            ));
        }

        let mut evidence: Vec<String> = stolen
            .iter()
            .map(|shim| {
                format!(
                    "command={} by={} ({}#{}) file={}",
                    shim.command, shim.by, shim.by_scope, shim.by_index, shim.file
                )
            })
            .collect();
        evidence.push(format!("shim-dir={shim_dir}"));
        evidence.push(format!("shim-dir-on-path={}", shims.on_path));
        evidence.push(format!("shim-commands={}", shims.commands.len()));

        out.push(Finding::new(
            ids::PATH_SHADOWED,
            Severity::Error,
            message,
            evidence,
            sources::PATH_RESOLUTION,
        ));
    }
}

/// 距 8191 悬崖的余量。
///
/// 四档映射（票据只写了"error（超限）/ info"，这里按"会不会突然全体失效"补齐）：
///
/// | 档 | 严重度 | 含义 |
/// |---|---|---|
/// | `ok` | **不报** | 余量充足。一条永远响的检查等于噪声 |
/// | `warning` | info | 到 75% 了，该清理重复条目了 |
/// | `critical` | warn | 到 90% 了，再加几条就要出事 |
/// | `exceeded` | **error** | 超过 8191，`cmd.exe` **已经完全不看 `PATH` 了** |
///
/// 8191 与 8192 的差别必须说准：`<= 8191` 还能用（`critical`），`> 8191` 是
/// `exceeded` —— 整条 `PATH` 一次性全部失效，不是"少几条命令"。
fn length_budget(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let budget = &facts.path.budget;
    let (severity, headline) = match budget.level.as_str() {
        "ok" => return,
        "warning" => (
            Severity::Info,
            "`PATH` 长度已经到了 8191 悬崖的 75% —— 该清理重复条目了",
        ),
        "critical" => (
            Severity::Warn,
            "`PATH` 长度已经到了 8191 悬崖的 90% —— 再加几条就要出事",
        ),
        _ => (
            Severity::Error,
            "`PATH` 长度**超过了 8191** —— `cmd.exe` 已经完全忽略它，\
             整条 `PATH` 一次性全部失效（不是少几条命令）",
        ),
    };

    let message = format!(
        "{headline}。当前：用户级 {} + 机器级 {} = 生效 {} 字符，悬崖 {}，还剩 {} 字符",
        budget.raw_user_chars,
        budget.raw_machine_chars,
        budget.effective_chars,
        budget.cliff,
        budget.remaining
    );
    let evidence = vec![
        format!("level={}", budget.level),
        format!("effective-chars={}", budget.effective_chars),
        format!("cliff={}", budget.cliff),
        format!("remaining={}", budget.remaining),
        format!("raw-user-chars={}", budget.raw_user_chars),
        format!("raw-machine-chars={}", budget.raw_machine_chars),
    ];

    out.push(Finding::new(
        ids::PATH_LENGTH_BUDGET,
        severity,
        message,
        evidence,
        sources::SYSTEM,
    ));
}

/// 空条目（`;;`、开头或结尾的 `;`）。
///
/// 为什么值得报：Windows 的历史行为是把空条目当"当前目录"。那既不是用户想要的，
/// 也是一个安全问题（从任意当前目录里执行同名程序）。判据是 `empty`，
/// **不是** `exists == no` —— 空条目的"在不在"这个问题本身不成立（决策 91）。
fn empty_entry(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts.path.entry.iter().filter(|row| row.empty).collect();
    if rows.is_empty() {
        return;
    }
    out.push(Finding::new(
        ids::PATH_EMPTY_ENTRY,
        Severity::Warn,
        format!(
            "`PATH` 里有 {} 段空条目。Windows 会把空条目当成\"当前目录\" —— \
             那既不是你要的，也意味着从任意当前目录里执行同名程序",
            rows.len()
        ),
        rows.iter()
            .map(|row| format!("{} <empty>", position(row)))
            .collect(),
        sources::REGISTRY,
    ));
}

/// 条目是 reparse point（junction / symlink / app-exec-alias）。
///
/// `info`：**它本身不是问题** —— 本项目的版本切换机制就是 junction（ADR-0001），
/// nvm4w 用的也是 symlink。报它是为了让"这个目录会跟着谁变"看得见。
fn reparse(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts
        .path
        .entry
        .iter()
        .filter(|row| row.reparse != ReparseKind::None)
        .collect();
    if rows.is_empty() {
        return;
    }

    let evidence: Vec<String> = rows
        .iter()
        .map(|row| match row.link_target.as_deref() {
            Some(target) => format!("{} {} -> {target}", position(row), row.reparse),
            None => format!("{} {}", position(row), row.reparse),
        })
        .collect();

    out.push(Finding::new(
        ids::PATH_REPARSE,
        Severity::Info,
        format!(
            "`PATH` 上有 {} 条条目是 reparse point（junction / symlink / alias）—— \
             它们本身不是问题，但它们会跟着别人变：切一次版本，这些目录的内容就换了",
            rows.len()
        ),
        evidence,
        sources::FILESYSTEM,
    ));
}

/// 相对路径条目。**这是 `error`**。
///
/// 为什么是 `error`：相对路径**永远**受 `MAX_PATH`(260) 限制（`\\?\` 无法加前缀），
/// 而且它的含义取决于"当前目录" —— 一个命令能不能找到，取决于你在哪里敲的。
///
/// 判据里刻意留了两条"不判"：空条目（不是一个目录）与含 `%VAR%` 的条目
/// （我们没展开它 —— `%FOO%\bin` 展开后可能是绝对的）。
fn relative(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts
        .path
        .entry
        .iter()
        .filter(|row| !row.empty && !row.has_vars && is_relative(&row.expanded))
        .collect();
    if rows.is_empty() {
        return;
    }
    out.push(Finding::new(
        ids::PATH_RELATIVE,
        Severity::Error,
        format!(
            r"`PATH` 上有 {} 条**相对路径**条目。相对路径永远受 `MAX_PATH`(260) 限制\
             （`\\?\` 无法加前缀），而且它的含义取决于你在哪个目录里敲命令",
            rows.len()
        ),
        rows.iter()
            .map(|row| format!("{} {}", position(row), row.raw))
            .collect(),
        sources::PATH_RESOLUTION,
    ));
}

/// 含非 ASCII 字符的条目。
///
/// `warn`：它在这台机器上工作得很好，但在换机器 / 换代码页 / 换 CI 时会咬人。
/// 本机 0 条 —— 所以这条检查的反例是"真实存在的健康状态"，不是编出来的。
fn non_ascii(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts
        .path
        .entry
        .iter()
        .filter(|row| !row.empty && !row.raw.is_ascii())
        .collect();
    if rows.is_empty() {
        return;
    }
    out.push(Finding::new(
        ids::PATH_NON_ASCII,
        Severity::Warn,
        format!(
            "`PATH` 上有 {} 条条目含非 ASCII 字符。它在这台机器上能用，\
             但换代码页 / 换 CI / 换机器时会咬人",
            rows.len()
        ),
        rows.iter()
            .map(|row| format!("{} {}", position(row), row.raw))
            .collect(),
        sources::PATH_RESOLUTION,
    ));
}

/// 含空格的条目。`info`。
///
/// 平台事实：`PATH` 上约 29% 的条目含空格（本机 48 条非空里有 14 条），
/// 而最糟的是同时含空格**和**版本号的那种。它本身不是问题（Windows 处理得了），
/// 只是所有"忘了给路径加引号"的脚本会在这里断掉。
fn spaces(facts: &MachineFacts, out: &mut Vec<Finding>) {
    let rows: Vec<&PathRow> = facts
        .path
        .entry
        .iter()
        .filter(|row| !row.empty && row.raw.contains(' '))
        .collect();
    if rows.is_empty() {
        return;
    }
    let total = facts.path.entry.iter().filter(|row| !row.empty).count();
    // `checked_div` 而不是 `if total == 0`：`total` 为 0 时 `None` 落回 0，
    // 而 clippy 的 `manual_checked_ops` 正好在提醒"这个 if 就是一次检查过的除法"。
    let percent = rows
        .len()
        .saturating_mul(100)
        .checked_div(total)
        .unwrap_or(0);
    out.push(Finding::new(
        ids::PATH_SPACES,
        Severity::Info,
        format!(
            "`PATH` 上有 {} 条条目含空格（{}/{} = {}%）。Windows 处理得了，\
             但所有忘了给路径加引号的脚本会在这里断掉",
            rows.len(),
            rows.len(),
            total,
            percent
        ),
        rows.iter()
            .map(|row| format!("{} {}", position(row), row.raw))
            .collect(),
        sources::PATH_RESOLUTION,
    ));
}

// ───────────────────────────── 判据助手 ─────────────────────────────

/// 条目的位置文本：`machine#21` / `user#3` / `process-only#8`。
///
/// **两套坐标系**（`PathRow::index` 对注册表作用域是"该作用域里的第几段"，
/// 对 `process-only` 是"进程 `PATH` 里的第几个"），所以前缀必须带上作用域 ——
/// 只写一个数字会让人去查错地方。
fn position(row: &PathRow) -> String {
    format!("{}#{}", scope_slug(row.scope), row.index)
}

fn scope_slug(scope: EnvScope) -> &'static str {
    match scope {
        EnvScope::Machine => "machine",
        EnvScope::User => "user",
        EnvScope::ProcessOnly => "process-only",
    }
}

/// 比较用的键：去引号、去首尾空白、去尾部反斜杠、折叠大小写。
///
/// 与 `tuoen_platform::path::normalize_entry` 的口径一致（那里是判重的唯一权威），
/// 但这里**不能直接调它** —— 检查项是纯函数，只吃事实。
fn compare_key(value: &str) -> String {
    value
        .trim()
        .trim_matches('"')
        .trim_end_matches('\\')
        .to_lowercase()
}

/// 这个值像不像一条**相对**路径。
///
/// 只有三种形态算绝对：`X:\…`、`\\server\share`、`/…`（`//server/share`）。
/// 其余非空的值都是相对的 —— 包括 `bin`、`.\bin`、`..\tools`。
fn is_relative(value: &str) -> bool {
    let text = value.trim().trim_matches('"').trim();
    if text.is_empty() {
        return false;
    }
    let bytes = text.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let unc = text.starts_with("\\\\") || text.starts_with("//");
    !(drive_absolute || unc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::files::{
        EntryRefRow, EnvFile, PathBudgetRow, PathFile, ToolsFile, WslFile,
    };
    use crate::doctor::facts::{self, ShadowFact, ShimFacts};

    /// 造一个只有 `PATH` 事实的机器 —— 这一族的十条检查只吃 `path` 与 `shims`。
    fn machine(path: PathFile) -> MachineFacts {
        MachineFacts {
            path,
            env: EnvFile::new(""),
            tools: ToolsFile::new(""),
            wsl: WslFile::new(""),
            system: facts::SystemFacts::default(),
            resolution: Vec::new(),
            global_prefix: Vec::new(),
            dev_roots: Vec::new(),
            shims: ShimFacts::default(),
        }
    }

    fn row(scope: EnvScope, index: usize, value: &str) -> PathRow {
        PathRow {
            scope,
            index,
            owner: "unknown".to_owned(),
            raw: value.to_owned(),
            expanded: value.to_owned(),
            quoted: false,
            empty: value.is_empty(),
            reg_type: None,
            // 含 `%VAR%` 的条目在真机上恒为 `unknown`（我们没展开它）——
            // fixture 必须照真机形状来，否则"含 `%` 的条目不判失效"这条判据
            // 会在测试里被绕过（第一版就是这样：`%NOPE%\bin` 被造成了 `Yes`）。
            exists: if value.is_empty() || value.contains('%') {
                Existence::Unknown
            } else {
                Existence::Yes
            },
            reparse: ReparseKind::None,
            link_target: None,
            has_vars: value.contains('%'),
            has_username: value.to_lowercase().contains(r"\users\"),
            dup_index: 0,
        }
    }

    fn file(rows: Vec<PathRow>) -> PathFile {
        let mut path = PathFile::new(
            "",
            PathBudgetRow {
                raw_user_chars: 0,
                raw_machine_chars: 0,
                effective_chars: 100,
                cliff: 8191,
                remaining: 8091,
                level: "ok".to_owned(),
            },
        );
        for row in rows {
            path.effective.push(EntryRefRow {
                scope: row.scope,
                index: row.index,
            });
            path.entry.push(row);
        }
        path
    }

    fn ids_of(findings: &[Finding]) -> Vec<&'static str> {
        findings.iter().map(|finding| finding.id).collect()
    }

    // ── path.duplicate ──────────────────────────────────────────────

    #[test]
    fn a_duplicate_is_found_even_when_the_case_and_trailing_slash_differ() {
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\shared"),
            row(EnvScope::Machine, 1, r"c:\SHARED\"),
            row(EnvScope::User, 0, r"C:\other"),
        ]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_DUPLICATE]);
        let finding = &findings[0];
        assert_eq!(finding.severity, Severity::Warn);
        assert_eq!(finding.evidence.len(), 1, "一组重复 = 一条 evidence");
        assert!(
            finding.evidence[0].contains("machine#0") && finding.evidence[0].contains("machine#1"),
            "两个位置都要点名：{:?}",
            finding.evidence
        );
        assert!(finding.message.contains("1 组"), "{}", finding.message);
        assert!(finding.message.contains("1 条富余"), "{}", finding.message);
    }

    #[test]
    fn distinct_values_produce_no_duplicate_finding() {
        // 反例：忽略大小写之后全都不同。
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\a"),
            row(EnvScope::Machine, 1, r"C:\b"),
            row(EnvScope::User, 0, r"C:\c"),
        ]));
        assert!(run(&facts).is_empty());
    }

    // ── path.missing ────────────────────────────────────────────────

    #[test]
    fn a_missing_directory_is_reported_and_the_unjudged_ones_are_counted() {
        let mut gone = row(EnvScope::Machine, 0, r"C:\gone");
        gone.exists = Existence::No;
        // 含 `%VAR%` 的条目：我们没展开，所以"在不在"是 unknown —— 不许报成失效。
        let unknown = row(EnvScope::User, 0, r"%NOPE%\bin");
        let facts = machine(file(vec![
            gone,
            unknown,
            row(EnvScope::User, 1, r"C:\here"),
        ]));

        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_MISSING]);
        assert_eq!(findings[0].evidence.len(), 1);
        assert!(findings[0].evidence[0].contains(r"C:\gone"));
        assert!(
            findings[0].message.contains("另有 1 条"),
            "分母要说出来：{}",
            findings[0].message
        );
    }

    #[test]
    fn an_entry_we_could_not_judge_never_becomes_a_missing_finding() {
        // 反例：全部条目都健康（含一条我们答不了的）。
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\here"),
            row(EnvScope::User, 0, r"%NOPE%\bin"),
        ]));
        assert!(run(&facts).is_empty());
    }

    // ── path.username-hardcoded ─────────────────────────────────────

    #[test]
    fn a_hardcoded_username_is_an_error_and_machine_scope_is_called_out() {
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\Users\someone\bin"),
            row(EnvScope::User, 0, r"C:\Users\someone\other"),
            row(EnvScope::User, 1, r"%USERPROFILE%\bin"),
        ]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_USERNAME_HARDCODED]);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].evidence.len(), 2);
        assert!(findings[0].message.contains("1 条在**机器级**"));
        assert!(findings[0].evidence[0].contains("(machine-scope)"));
    }

    #[test]
    fn a_portable_spelling_is_not_a_hardcoded_username() {
        // 反例：`%USERPROFILE%` 是可搬运的写法。
        let facts = machine(file(vec![row(EnvScope::User, 0, r"%USERPROFILE%\bin")]));
        assert!(run(&facts).is_empty());
    }

    // ── path.shadowed ───────────────────────────────────────────────

    fn shadow(command: &str, by: &str, file: &str) -> ShadowFact {
        ShadowFact {
            command: command.to_owned(),
            by: by.to_owned(),
            by_scope: "machine",
            by_index: 0,
            file: file.to_owned(),
        }
    }

    #[test]
    fn a_shadowed_command_is_reported_per_shadowing_directory() {
        let mut facts = machine(file(vec![row(EnvScope::Machine, 0, r"C:\nvm4w\nodejs")]));
        facts.shims = ShimFacts {
            dir: Some(r"C:\Users\me\AppData\Local\tuoen\shims".to_owned()),
            on_path: true,
            commands: vec!["node".to_owned(), "npm".to_owned()],
            shadowed: vec![
                shadow("node", r"C:\nvm4w\nodejs", "node.exe"),
                shadow("npm", r"C:\nvm4w\nodejs", "npm.cmd"),
            ],
        };
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_SHADOWED]);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].evidence.len(), 5, "2 条命令 + 3 条分母");
        assert!(findings[0].message.contains("2 条 shim 里有 2 条"));
        assert!(
            !findings[0].message.contains("根因"),
            "目录在 PATH 上就不该说根因"
        );
    }

    #[test]
    fn two_shadowing_directories_produce_two_findings() {
        let mut facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\nvm4w\nodejs"),
            row(
                EnvScope::Machine,
                1,
                r"C:\Program Files (x86)\Common Files\Oracle\Java\java8path",
            ),
        ]));
        facts.shims = ShimFacts {
            dir: Some(r"C:\shims".to_owned()),
            on_path: true,
            commands: vec!["node".to_owned(), "java".to_owned()],
            shadowed: vec![
                shadow("node", r"C:\nvm4w\nodejs", "node.exe"),
                shadow(
                    "java",
                    r"C:\Program Files (x86)\Common Files\Oracle\Java\java8path",
                    "java.exe",
                ),
            ],
        };
        let findings = run(&facts);
        // fixture 里那个 Oracle 路径含空格，所以这里只数 `path.shadowed` 的条数。
        let shadowed: Vec<&Finding> = findings
            .iter()
            .filter(|finding| finding.id == ids::PATH_SHADOWED)
            .collect();
        assert_eq!(shadowed.len(), 2, "两个遮蔽目录 = 两条发现");
        // 顺序由 BTreeMap 决定（大写字母排在小写前面），所以按内容断言而不是按下标。
        assert!(
            shadowed
                .iter()
                .any(|f| f.message.contains(r"C:\nvm4w\nodejs"))
        );
        assert!(shadowed.iter().any(|f| f.message.contains("java8path")));
    }

    #[test]
    fn a_shim_directory_that_is_not_on_path_says_so_first() {
        let mut facts = machine(file(vec![row(EnvScope::Machine, 0, r"C:\other")]));
        facts.shims = ShimFacts {
            dir: Some(r"C:\shims".to_owned()),
            on_path: false,
            commands: vec!["node".to_owned()],
            shadowed: vec![shadow("node", r"C:\other", "node.exe")],
        };
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_SHADOWED]);
        assert!(
            findings[0].message.contains("根因"),
            "目录不在 PATH 上时根因要写在最前：{}",
            findings[0].message
        );
    }

    #[test]
    fn no_shims_means_nothing_to_shadow_and_nothing_is_reported() {
        // **反例（也是决策 75 的教训）**：盘上一条 shim 都没有时，
        // `shadowed = 0` 不是"没被遮蔽"，而是"没得比" —— 不许报。
        let facts = machine(file(vec![row(EnvScope::Machine, 0, r"C:\nvm4w\nodejs")]));
        assert!(run(&facts).is_empty());
    }

    // ── path.length-budget ──────────────────────────────────────────

    #[test]
    fn the_budget_boundaries_are_exactly_what_the_ticket_asked_for() {
        // 票据要求断言 1719 / 8190 / 8191 / 8192 四个值的行为差异。
        let cases = [
            (1719, "ok", None),
            (8190, "critical", Some(Severity::Warn)),
            (8191, "critical", Some(Severity::Warn)),
            (8192, "exceeded", Some(Severity::Error)),
        ];
        for (chars, level, expected) in cases {
            let mut path = file(Vec::new());
            path.budget = PathBudgetRow {
                raw_user_chars: 0,
                raw_machine_chars: 0,
                effective_chars: chars,
                cliff: 8191,
                remaining: 8191usize.saturating_sub(chars),
                level: level.to_owned(),
            };
            let findings = run(&machine(path));
            match expected {
                None => assert!(findings.is_empty(), "{chars} 字符（{level}）不该报任何东西"),
                Some(severity) => {
                    assert_eq!(ids_of(&findings), vec![ids::PATH_LENGTH_BUDGET], "{chars}");
                    assert_eq!(findings[0].severity, severity, "{chars} 字符");
                }
            }
        }
    }

    #[test]
    fn the_warning_tier_is_info_and_still_fires() {
        let mut path = file(Vec::new());
        path.budget = PathBudgetRow {
            raw_user_chars: 0,
            raw_machine_chars: 0,
            effective_chars: 7000,
            cliff: 8191,
            remaining: 1191,
            level: "warning".to_owned(),
        };
        let findings = run(&machine(path));
        assert_eq!(ids_of(&findings), vec![ids::PATH_LENGTH_BUDGET]);
        assert_eq!(findings[0].severity, Severity::Info);
    }

    #[test]
    fn exceeding_the_cliff_says_the_whole_path_stops_working() {
        let mut path = file(Vec::new());
        path.budget = PathBudgetRow {
            raw_user_chars: 0,
            raw_machine_chars: 0,
            effective_chars: 8192,
            cliff: 8191,
            remaining: 0,
            level: "exceeded".to_owned(),
        };
        let findings = run(&machine(path));
        assert_eq!(findings[0].severity, Severity::Error);
        assert!(
            findings[0].message.contains("整条 `PATH` 一次性全部失效"),
            "{}",
            findings[0].message
        );
    }

    // ── path.empty-entry ────────────────────────────────────────────

    #[test]
    fn an_empty_entry_is_reported_as_an_empty_entry_not_as_a_missing_directory() {
        let mut facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\here"),
            row(EnvScope::Machine, 1, ""),
            row(EnvScope::Machine, 2, ""),
        ]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_EMPTY_ENTRY]);
        assert_eq!(
            findings[0].evidence,
            vec!["machine#1 <empty>", "machine#2 <empty>"]
        );
        assert!(!ids_of(&findings).contains(&ids::PATH_MISSING));

        // 反例：没有空条目就没有这条发现。
        facts.path.entry.retain(|row| !row.empty);
        assert!(run(&facts).is_empty());
    }

    // ── path.reparse ────────────────────────────────────────────────

    #[test]
    fn a_reparse_entry_is_info_and_carries_its_target() {
        let mut link = row(EnvScope::Machine, 0, r"C:\nvm4w\nodejs");
        link.reparse = ReparseKind::SymlinkDir;
        link.link_target = Some(r"C:\Users\me\AppData\Local\nvm\v24.19.0".to_owned());
        let facts = machine(file(vec![link, row(EnvScope::User, 0, r"C:\plain")]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_REPARSE]);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(findings[0].evidence[0].contains("symlink-dir -> "));
    }

    #[test]
    fn plain_directories_produce_no_reparse_finding() {
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\a"),
            row(EnvScope::User, 0, r"C:\b"),
        ]));
        assert!(run(&facts).is_empty());
    }

    // ── path.relative ───────────────────────────────────────────────

    #[test]
    fn a_relative_entry_is_an_error_and_the_three_absolute_shapes_are_not() {
        let facts = machine(file(vec![
            row(EnvScope::User, 0, "bin"),
            row(EnvScope::User, 1, r".\bin"),
            row(EnvScope::User, 2, r"..\tools"),
            row(EnvScope::Machine, 0, r"C:\absolute"),
            row(EnvScope::Machine, 1, r"\\server\share"),
            // `/posix-style` 是**根相对**（跟着当前驱动器走），所以它也算相对。
            row(EnvScope::Machine, 2, "/posix-style"),
            row(EnvScope::Machine, 3, r"%DRIVE%\bin"),
        ]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_RELATIVE]);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(
            findings[0].evidence.len(),
            4,
            "bin / .\\bin / ..\\tools / /posix-style 都是相对的（`/x` 是根相对）"
        );
        assert!(
            !findings[0].evidence.iter().any(|e| e.contains("%DRIVE%")),
            "含 `%VAR%` 的条目不判 —— 展开后可能是绝对的：{:?}",
            findings[0].evidence
        );
    }

    #[test]
    fn only_absolute_entries_produce_no_relative_finding() {
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\a"),
            row(EnvScope::Machine, 1, r"\\server\share"),
        ]));
        assert!(run(&facts).is_empty());
    }

    // ── path.non-ascii ──────────────────────────────────────────────

    #[test]
    fn a_non_ascii_entry_is_a_warning() {
        let facts = machine(file(vec![
            row(EnvScope::User, 0, "C:\\工具\\bin"),
            row(EnvScope::User, 1, r"C:\plain"),
        ]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_NON_ASCII]);
        assert_eq!(findings[0].severity, Severity::Warn);
        assert_eq!(findings[0].evidence.len(), 1);
    }

    #[test]
    fn ascii_entries_produce_no_non_ascii_finding() {
        let facts = machine(file(vec![row(EnvScope::User, 0, r"C:\plain")]));
        assert!(run(&facts).is_empty());
    }

    // ── path.spaces ─────────────────────────────────────────────────

    #[test]
    fn entries_with_spaces_are_info_and_the_percentage_is_reported() {
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\Program Files\Git\cmd"),
            row(EnvScope::Machine, 1, r"C:\plain"),
            row(EnvScope::Machine, 2, r"C:\plain2"),
        ]));
        let findings = run(&facts);
        assert_eq!(ids_of(&findings), vec![ids::PATH_SPACES]);
        assert_eq!(findings[0].severity, Severity::Info);
        assert!(
            findings[0].message.contains("1/3 = 33%"),
            "分母要算对：{}",
            findings[0].message
        );
    }

    #[test]
    fn entries_without_spaces_produce_no_spaces_finding() {
        let facts = machine(file(vec![row(EnvScope::Machine, 0, r"C:\plain")]));
        assert!(run(&facts).is_empty());
    }

    // ── 全局：位置文本与"健康机器什么都不报" ─────────────────────────

    #[test]
    fn positions_carry_the_scope_because_there_are_two_coordinate_systems() {
        assert_eq!(position(&row(EnvScope::Machine, 3, "x")), "machine#3");
        assert_eq!(position(&row(EnvScope::User, 0, "x")), "user#0");
        assert_eq!(
            position(&row(EnvScope::ProcessOnly, 8, "x")),
            "process-only#8"
        );
    }

    #[test]
    fn a_healthy_machine_produces_nothing_at_all() {
        // 这一条是这一族的总反例：一个干净的 `PATH` 不该产生任何一条发现。
        let facts = machine(file(vec![
            row(EnvScope::Machine, 0, r"C:\Windows\System32"),
            row(EnvScope::Machine, 1, r"C:\Windows"),
            row(EnvScope::User, 0, r"%USERPROFILE%\bin"),
        ]));
        assert!(run(&facts).is_empty());
    }
}
