//! `PATH` 的读、分析与改写。
//!
//! # 为什么这个模块存在，以及它为什么这么小心
//!
//! `PATH` 是本项目里**唯一**一处「一次调用就能让整台机器所有命令失效」的地方：
//!
//! - `setx` 在 **1024** 字符处**静默裁剪**，并且会**永久展开**所有 `%VAR%` 引用。
//!   本机实测（`docs/acceptance/L0-08-path-machine.txt`）：用户级 773 + 机器级 1007
//!   = **1780** 字符，`setx PATH "%PATH%;…"` 会既裁剪又把整条机器 `PATH`
//!   永久复制进 `HKCU`。**本项目绝不调用 `setx`。**
//!   （这条算术**不是**常量：票据写作时是 725 + 978 = 1703，机器级 `Path` 的类型
//!   从 `REG_EXPAND_SZ` 变成了 `REG_SZ`，长度也变了。所以它只作示例，不作断言。）
//! - `cmd.exe` 在 `PATH` 超过 **8191** 字符后**完全忽略它** —— 不是"部分失效"，
//!   是整条 `PATH` 一次性全部失效，所有命令都报"不是内部或外部命令"。
//!   这两个数字是**不同的悬崖**，混为一谈会让告警出现在错误的位置上。
//!
//! 所以这里的形状是「**先出计划，再落盘**」：
//!
//! 1. [`analyze`] 只读，产出 [`PathAnalysis`]（长度预算、重复、失效条目、用户名依赖、
//!    遮蔽了哪些 shim）；
//! 2. [`plan_add`] / [`plan_remove`] 产出 [`PathPlan`]（改之前是什么、改之后是什么、
//!    类型会不会变、具体改了哪几条）；
//! 3. [`apply`] 才写注册表，写完整条值**一次**，然后广播 `WM_SETTINGCHANGE`。
//!
//! `--dry-run` 走的是**同一套** `plan_*`，只是不调 `apply` —— 不是另写一条只读分支。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::PATH_CLIFF_CMD;
use crate::env_block::{EnvBlock, EnvScope, ProcessEnv, RegType, USER_ENV_SUBKEY};
use crate::error::PlatformError;
use crate::fs_facts::FileSystem;
use crate::registry::{RegHive, RegValue, Registry};

/// 环境变量名。**大小写不敏感**（Windows 上 `Path` 与 `PATH` 是同一个变量），
/// 而注册表里本机用的就是 `Path` 这个写法。
pub const PATH_NAME: &str = "Path";

/// 写 `PATH` 时使用的、在 `PATH` 上找命令的扩展名，**按 Windows 的解析顺序**。
///
/// 顺序有意义：同一个目录里同时有 `node.exe` 与 `node.cmd` 时，先命中的是 `.exe`。
pub const PATH_EXTENSIONS: &[&str] = &["exe", "com", "bat", "cmd", "ps1"];

/// 预算告警的档位。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BudgetLevel {
    /// 还有余量。
    Ok,
    /// 已用 ≥ 75%。
    Warning,
    /// 已用 ≥ 90%。
    Critical,
    /// **已超过 8191** —— `cmd.exe` 现在完全忽略整条 `PATH`。
    Exceeded,
}

impl BudgetLevel {
    /// 稳定小写 slug（进 `--json`，不本地化）。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Warning => "warning",
            Self::Critical => "critical",
            Self::Exceeded => "exceeded",
        }
    }

    /// 是否需要在人类输出里显眼地告警。
    #[must_use]
    pub const fn is_alarming(self) -> bool {
        matches!(self, Self::Critical | Self::Exceeded)
    }
}

/// `PATH` 长度预算。
///
/// **以「生效的 `PATH`」为准**，不是以某一个作用域为准：`cmd.exe` 看的是进程环境块里
/// 那一条，而它是「机器级 + 用户级 + 进程注入」拼起来的。本机实测：注册表里
/// 978 + 725 = 1703，而进程里实际是 1781（差的 78 是 PowerShell 的 MSIX 别名注入的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathBudget {
    /// 生效 `PATH` 的字符数。
    pub effective_chars: usize,
    /// 机器级 + 用户级注册表原文的字符数。**与 `effective_chars` 可能不等**。
    pub registry_chars: usize,
    /// 悬崖（[`PATH_CLIFF_CMD`]）。
    pub cliff: usize,
    /// 还剩多少字符（[`BudgetLevel::Exceeded`] 时为 0，不做回绕）。
    pub remaining: usize,
    pub level: BudgetLevel,
}

impl PathBudget {
    /// 按生效长度算档位。
    #[must_use]
    pub fn of(effective_chars: usize, registry_chars: usize) -> Self {
        let level = if effective_chars > PATH_CLIFF_CMD {
            BudgetLevel::Exceeded
        } else if effective_chars * 100 >= PATH_CLIFF_CMD * 90 {
            BudgetLevel::Critical
        } else if effective_chars * 100 >= PATH_CLIFF_CMD * 75 {
            BudgetLevel::Warning
        } else {
            BudgetLevel::Ok
        };
        Self {
            effective_chars,
            registry_chars,
            cliff: PATH_CLIFF_CMD,
            remaining: PATH_CLIFF_CMD.saturating_sub(effective_chars),
            level,
        }
    }
}

/// `PATH` 里的一条条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathEntry {
    /// 原文，**原样保留**（含引号与可能的首尾空白）。
    pub raw: String,
    /// 用于比较的规范形态：去掉外层引号与首尾空白，去掉结尾的反斜杠（`C:\` 除外）。
    pub value: String,
    /// 原文是不是被一对引号包着。
    pub quoted: bool,
}

impl PathEntry {
    /// 比较用的键：大小写折叠后的 [`Self::value`]。
    #[must_use]
    pub fn key(&self) -> String {
        self.value.to_lowercase()
    }

    /// 空条目（`;;`、结尾的 `;`）。**这是真实存在的**：本机机器级 `Path` 里就有。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }
}

/// 一个作用域里的 `PATH`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopedPath {
    pub scope: EnvScope,
    /// 注册表里的原文（**未展开**）。
    pub raw: String,
    /// 注册表里的类型。`None` 表示这个作用域里没有 `Path`（或读不到）。
    pub reg_type: Option<RegType>,
    pub entries: Vec<PathEntry>,
    /// `raw` 的字符数。
    pub chars: usize,
    /// 空条目出现的位置（在原条目序列里的下标）。
    pub empty_positions: Vec<usize>,
}

/// 指向某个作用域里的某条条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRef {
    pub scope: EnvScope,
    /// **两套坐标系，先读 `scope` 再解释这个数。**
    ///
    /// - `Machine` / `User`：该作用域 `entries` 序列里的下标（**含空条目**，
    ///   所以它就是原文里的第几段）。与 [`ScopedPath::entries`] 的下标一一对应。
    /// - `ProcessOnly`：**进程 `PATH`** 里的下标。那些条目不在任何注册表里，
    ///   拿它去索引 `scopes` 会得到另一个作用域的东西。
    ///
    /// 一个字段承担两套坐标是一种欠债，但两种"出处"在这里都只需要一个可复算的位置，
    /// 而区分它们的信息已经在 `scope` 里。**消费方必须先看 `scope`。**
    pub index: usize,
    pub raw: String,
    pub value: String,
}

/// 同一个目录出现了多次。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Duplicate {
    /// 大小写折叠后的值，作为分组键。
    pub key: String,
    /// 按生效顺序排列的每一次出现。
    pub at: Vec<EntryRef>,
}

/// 指向一个不存在的目录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DanglingEntry {
    pub entry: EntryRef,
    /// 该条目是不是依赖变量展开才能成立（含 `%`）。
    pub uses_variable: bool,
}

/// 一条硬编码了用户名的条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsernameDependency {
    pub entry: EntryRef,
    /// 被硬编码进去的用户名。
    pub name: String,
    /// **这条在机器级**吗。机器级的硬编码用户名最危险：换账号名之后它静默失效，
    /// 而在这台机器上 `Test-Path` 仍然通过（因为那个目录确实叫这个名字）。
    pub at_machine_scope: bool,
}

/// 我们发布的一条命令被别的条目抢在了前面。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowedShim {
    /// 命令名（不含扩展名），例如 `node`。
    pub command: String,
    /// 抢在我们前面的那一条。
    pub by: EntryRef,
    /// 那条目录里实际命中的文件（含扩展名）。
    pub file: String,
    /// 我们自己的 shim 目录。
    pub shim_dir: String,
}

/// 一次完整分析的结果。**纯只读**。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathAnalysis {
    /// 两个注册表作用域（`[0]` 机器级、`[1]` 用户级）。**不是全部事实** —— 见 `effective`。
    pub scopes: Vec<ScopedPath>,
    /// **真实的生效顺序**，逐条标注它来自哪个作用域。
    ///
    /// 以**进程里那一条 `PATH`** 为准，因为那才是 Windows 真正拼出来的东西
    /// （`CreateProcess` 复制的是**父进程**的环境块，不是注册表）。真机实测的形状：
    ///
    /// ```text
    /// [启动器注入] + 机器级 + 用户级          （一个 shell 从注册表建立环境时的顺序）
    /// ```
    ///
    /// 本机实测：PowerShell 的 MSIX 别名目录排在**最前面**（在机器级之前），
    /// 而 `cargo` / `rustup` / 宿主各自又往前面插自己的目录。所以
    /// "机器级在前、用户级在后"**只是这个顺序的一部分** —— 而它成立的那一半
    /// （机器级条目全部排在用户级条目之前）正是 ADR-0002"用户级工具永远输掉名字冲突"
    /// 的依据，这条实测成立。
    ///
    /// 注册表里有、进程里没有的条目**追加在末尾**：成因是"这个终端在改动之前就开了"
    /// （环境块只在 `CreateProcess` 时复制），而它们仍然要参与遮蔽判定 ——
    /// 不能因为进程里看不见就当它不存在。
    ///
    /// # 它**按大小写折叠后的值去重过**
    ///
    /// 同一个目录在生效顺序里出现两次，第二次没有意义（第一次就已经赢了名字冲突），
    /// 所以这里只留第一次出现。因此 `effective.len()` 是**不同目录的个数**，
    /// 不是条目总数（那是 [`Self::scopes`] 的事）。
    ///
    /// 当进程 `PATH` 正好是"机器级 + 用户级"那份合并结果时，有一条可核对的关系：
    ///
    /// ```text
    /// effective.len() + （重复组各自的富余量之和） == 进程 PATH 里的非空条目数
    /// ```
    ///
    /// 本机实测：进程 `PATH` 50 条、`effective` 32 条、富余量 18 —— 32 + 18 = 50。
    /// **进程里有启动器注入时这条关系不成立**（注入项不在注册表里，不产生富余量），
    /// 所以它是个可观察的现象，不是一条可以拿去断言的恒等式。
    pub effective: Vec<EntryRef>,
    pub budget: PathBudget,
    /// 进程 `PATH` 里有、两个作用域都没有的条目 —— 进程注入项。
    /// **它们不在任何注册表里**，本机实测 PowerShell 的 MSIX 别名就是这样。
    pub process_only: Vec<String>,
    pub duplicates: Vec<Duplicate>,
    pub dangling: Vec<DanglingEntry>,
    pub username_dependencies: Vec<UsernameDependency>,
    pub shadowed_shims: Vec<ShadowedShim>,
    /// 我们的 shim 目录里发布了哪些命令（`<名字>.exe`，已排序去重）。
    ///
    /// **它是 [`Self::shadowed_shims`] 的分母**：这里为空时那条列表必然为空，
    /// 但那不是"没输"，是"没得比"。本机实测：真的生成过 shim 之后这里是
    /// `["corepack", "node", "npm", "npx"]`。
    pub shim_commands: Vec<String>,
}

impl PathAnalysis {
    /// [`Self::effective`] 的克隆：**真实的生效顺序**，逐条带出处。
    ///
    /// 它已经**去重**，而且**以进程注入项开头**（本机实测：PowerShell 的 MSIX 别名目录
    /// 排在机器级之前）。"机器级在前、用户级在后"是这个顺序的**一部分**，不是它的定义 ——
    /// 完整的定义与去重规则见 [`Self::effective`] 的字段文档。
    #[must_use]
    pub fn effective_entries(&self) -> Vec<EntryRef> {
        self.effective.clone()
    }
}

/// 把一条 `PATH` 原文切成条目。
///
/// **`;` 永远分隔，引号不保护它** —— 这一点与 shell 的直觉相反，但 Windows 在
/// 解析 `PATH` 时就是先按 `;` 切、再决定要不要剥引号（"引号只是用来保护空格"）。
/// 空段**保留**成空条目而不是丢掉：本机机器级 `Path` 里真的有 `;;`，
/// 而"我们读的时候把它吃掉了"会让写回时静默改写用户的 `PATH`。
#[must_use]
pub fn parse_entries(raw: &str) -> Vec<PathEntry> {
    raw.split(';')
        .map(|part| {
            let trimmed = part.trim();
            let (inner, quoted) = match trimmed
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
            {
                Some(inner) => (inner, true),
                None => (trimmed, false),
            };
            PathEntry {
                raw: part.to_owned(),
                value: normalize_entry(inner),
                quoted,
            }
        })
        .collect()
}

/// 条目的比较形态：去首尾空白、去结尾反斜杠（但保留 `C:\` 这种根）。
pub fn normalize_entry(text: &str) -> String {
    let trimmed = text.trim();
    // 只去掉**多余的**结尾反斜杠：`C:\` 是根，`C:\a\` 等价于 `C:\a`。
    let mut end = trimmed.len();
    while end > 0 && (trimmed.as_bytes()[end - 1] == b'\\' || trimmed.as_bytes()[end - 1] == b'/') {
        // 保留 `X:\` 里的那一个。
        if end == 3 && trimmed.as_bytes()[1] == b':' {
            break;
        }
        end -= 1;
    }
    trimmed[..end].to_owned()
}

/// 从一条条目里找出硬编码的用户名。
///
/// 判据是「路径里出现 `\Users\<名字>\` 且那个名字不是 `%VAR%`」。
/// 只看 `\Users\` 是因为那是 Windows 唯一稳定的用户目录父路径（本机 11 条，
/// 其中 2 条在机器级）。
#[must_use]
pub fn hardcoded_username(value: &str) -> Option<String> {
    let lowered = value.to_lowercase();
    let at = lowered.find(r"\users\")?;
    let rest = &value[at + r"\users\".len()..];
    if rest.is_empty() {
        return None;
    }
    // 到下一个分隔符为止。
    let name = match rest.find(['\\', '/']) {
        Some(end) => &rest[..end],
        None => rest,
    };
    let name = name.trim();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    // `%USERNAME%` / `%USERPROFILE%` 这类是**可移植**的，不算硬编码。
    if name.contains('%') {
        return None;
    }
    Some(name.to_owned())
}

/// 读两个作用域并做完整分析。
///
/// **不写任何东西。** `shim_dir` 给了就做遮蔽检测，`None` 就跳过。
#[must_use]
pub fn analyze<E, P, F>(block: &E, process: &P, fs: &F, shim_dir: Option<&Path>) -> PathAnalysis
where
    E: EnvBlock,
    P: ProcessEnv,
    F: FileSystem,
{
    let mut scopes = Vec::new();
    for scope in [EnvScope::Machine, EnvScope::User] {
        let found = block.get(scope, PATH_NAME);
        let raw = found
            .as_ref()
            .map(|v| v.value_raw.clone())
            .unwrap_or_default();
        let entries = parse_entries(&raw);
        let empty_positions = entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.is_empty())
            .map(|(i, _)| i)
            .collect();
        scopes.push(ScopedPath {
            scope,
            chars: raw.chars().count(),
            raw,
            reg_type: found.as_ref().map(|v| v.reg_type),
            entries,
            empty_positions,
        });
    }

    let process_entries = process.path_entries();
    let effective_chars = process_entries
        .iter()
        .map(|e| e.chars().count())
        .sum::<usize>()
        + process_entries.len().saturating_sub(1); // 分隔符
    let registry_chars = scopes.iter().map(|s| s.chars).sum::<usize>();

    // ── 真实的生效顺序 ───────────────────────────────────────────────
    //
    // 以进程里那一条为准。本机实测：`[PowerShell 的 MSIX 别名注入] + 机器级 + 用户级`
    // —— **注入项排在机器级前面**，所以"机器级在前、用户级在后"是不完整的读法。
    // 注册表里有、进程里没有的（这个终端在改动之前就开了）追加在末尾：它们仍然要
    // 参与遮蔽判定，只是位置只能算在最后。
    let registry_lookup: Vec<(String, EnvScope, usize, String)> = scopes
        .iter()
        .flat_map(|scope| {
            scope
                .entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| !entry.is_empty())
                .map(move |(index, entry)| (entry.key(), scope.scope, index, entry.raw.clone()))
        })
        .collect();
    let mut effective: Vec<EntryRef> = Vec::new();
    let mut placed: Vec<String> = Vec::new();
    let mut process_only = Vec::new();
    for (index, raw) in process_entries.iter().enumerate() {
        let value = normalize_entry(raw.trim_matches('"'));
        if value.is_empty() {
            continue;
        }
        let key = value.to_lowercase();
        // 同一个目录在生效顺序里出现两次，第二次没有意义（第一次就已经赢了）。
        if placed.contains(&key) {
            continue;
        }
        placed.push(key.clone());
        match registry_lookup.iter().find(|(k, _, _, _)| *k == key) {
            // 在注册表里 → 用注册表那份原文与出处（去重之后仍然指得回原文）。
            Some((_, scope, at, registry_raw)) => effective.push(EntryRef {
                scope: *scope,
                index: *at,
                raw: registry_raw.clone(),
                value,
            }),
            // 不在 → 进程注入项。**它排在机器级前面**，所以遮蔽判定必须看得见它。
            None => {
                process_only.push(value.clone());
                effective.push(EntryRef {
                    scope: EnvScope::ProcessOnly,
                    index,
                    raw: raw.clone(),
                    value,
                });
            }
        }
    }
    for (key, scope, index, registry_raw) in &registry_lookup {
        if placed.contains(key) {
            continue;
        }
        placed.push(key.clone());
        effective.push(EntryRef {
            scope: *scope,
            index: *index,
            raw: registry_raw.clone(),
            value: normalize_entry(registry_raw.trim_matches('"')),
        });
    }

    // ── 重复、失效、用户名 ───────────────────────────────────────────
    let mut groups: BTreeMap<String, Vec<EntryRef>> = BTreeMap::new();
    let mut dangling = Vec::new();
    let mut username_dependencies = Vec::new();
    for scope in &scopes {
        for (index, entry) in scope.entries.iter().enumerate() {
            if entry.is_empty() {
                continue;
            }
            let reference = EntryRef {
                scope: scope.scope,
                index,
                raw: entry.raw.clone(),
                value: entry.value.clone(),
            };
            groups
                .entry(entry.key())
                .or_default()
                .push(reference.clone());

            // 含 `%` 的条目不判"失效"：我们**不展开**用户的值（写回会破坏往返），
            // 所以在这里它可能只是"要展开之后才存在"。报成失效是假阳性。
            let uses_variable = entry.value.contains('%');
            if !uses_variable && !fs.inspect(Path::new(&entry.value)).exists {
                dangling.push(DanglingEntry {
                    entry: reference.clone(),
                    uses_variable,
                });
            }
            if let Some(name) = hardcoded_username(&entry.value) {
                username_dependencies.push(UsernameDependency {
                    entry: reference,
                    name,
                    at_machine_scope: scope.scope == EnvScope::Machine,
                });
            }
        }
    }
    let duplicates: Vec<Duplicate> = groups
        .into_iter()
        .filter(|(_, at)| at.len() > 1)
        .map(|(key, at)| Duplicate { key, at })
        .collect();

    // ── 遮蔽 ─────────────────────────────────────────────────────────
    let (shadowed_shims, shim_commands) = shim_dir.map_or_else(
        || (Vec::new(), Vec::new()),
        |dir| detect_shadowing(fs, &effective, dir),
    );

    PathAnalysis {
        scopes,
        effective,
        budget: PathBudget::of(effective_chars, registry_chars),
        process_only,
        duplicates,
        dangling,
        username_dependencies,
        shadowed_shims,
        shim_commands,
    }
}

/// 我们的哪些命令被抢在了前面。
///
/// 走**真实的生效顺序**（`effective`），对每条命令取第一个命中它的目录；
/// 命中目录不是我们的 shim 目录就是被遮蔽了。`by` 里带着那条条目的出处
/// —— 可能是机器级、用户级，也可能是**进程注入**的（它排在机器级前面）。
fn detect_shadowing<F: FileSystem>(
    fs: &F,
    effective: &[EntryRef],
    shim_dir: &Path,
) -> (Vec<ShadowedShim>, Vec<String>) {
    let commands = shim_commands(fs, shim_dir);
    if commands.is_empty() {
        return (Vec::new(), commands);
    }
    let shim_key = normalize_entry(&shim_dir.to_string_lossy()).to_lowercase();

    // 每个目录只列一次。
    let mut listed: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut out = Vec::new();
    // 对**每条命令**走一遍生效顺序，第一个命中它的目录说了算：
    // 到我们自己的 shim 目录 = 没被遮蔽；在那之前命中 = 被那个目录遮蔽。
    'command: for command in &commands {
        for entry in effective {
            let key = entry.value.to_lowercase();
            if key == shim_key {
                continue 'command;
            }
            let names = listed.entry(key.clone()).or_insert_with(|| {
                fs.list_dir(Path::new(&entry.value))
                    .into_iter()
                    .map(|e| e.name.to_lowercase())
                    .collect()
            });
            for extension in PATH_EXTENSIONS {
                let file = format!("{command}.{extension}");
                if names.contains(&file) {
                    out.push(ShadowedShim {
                        command: command.clone(),
                        by: entry.clone(),
                        file,
                        shim_dir: shim_dir.to_string_lossy().into_owned(),
                    });
                    continue 'command;
                }
            }
        }
    }
    (out, commands)
}

/// 我们的 shim 目录里发布了哪些命令（`<名字>.exe`）。
///
/// 它同时是**遮蔽检测的分母**：空 vec 意味着"我们一条命令都还没发布"，
/// 那时 `shadowed_shims` 为空**不是因为没输**，而是因为**没得比**。
/// 消费方要分得开这两件事（曾写出过"每条命令都是第一个被命中的"这种
/// 在空目录上毫无意义的结论）。
#[must_use]
pub fn shim_commands<F: FileSystem>(fs: &F, shim_dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs
        .list_dir(shim_dir)
        .into_iter()
        .filter_map(|entry| {
            let lowered = entry.name.to_lowercase();
            let stem = lowered.strip_suffix(".exe")?;
            if stem.is_empty() {
                None
            } else {
                Some(stem.to_owned())
            }
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

// ─────────────────────────── 计划与落盘 ───────────────────────────

/// 计划里的一条改动。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PathChange {
    /// 追加一条。
    Add {
        value: String,
        /// 追加到第几段（0 起）。
        at: usize,
    },
    /// 删掉一条。
    Remove {
        value: String,
        /// 原来在第几段。
        was_at: usize,
    },
    /// 什么都不做，以及为什么。
    Noop { value: String, reason: NoopReason },
    /// 值的注册表类型会变。
    Retype { from: RegType, to: RegType },
    /// 顺带清掉了空条目。
    DropEmptySegments { count: usize },
}

/// [`PathChange::Noop`] 的原因。**稳定 slug**，不本地化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NoopReason {
    /// 已经在里面了（按大小写不敏感比较）。
    AlreadyPresent,
    /// 本来就不在里面。
    Absent,
    /// 是我们自己的 shim 目录，**不许删**。
    ProtectedShimDir,
}

/// 对用户级 `PATH` 的一次修改计划。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathPlan {
    pub scope: EnvScope,
    /// 改之前的原文。
    pub before_raw: String,
    /// 改之后的原文。
    pub after_raw: String,
    pub before_type: RegType,
    pub after_type: RegType,
    pub changes: Vec<PathChange>,
    /// 值会不会真的变。`false` 时 `apply` 什么都不做。
    pub changes_value: bool,
    /// 写回之后的长度预算。**注册表口径的下界**。
    ///
    /// 它是「机器级 + 用户级写回后」的长度，不含**进程注入**的那些条目
    /// （本机有 78 字符是 PowerShell 的 MSIX 别名注入的），所以真实的生效长度
    /// 只会比它更长、只会比它更早撞悬崖。拿它当"至少这么长"读，不要当精确值。
    pub budget_after: PathBudget,
}

impl PathPlan {
    /// 这个计划会真的写东西吗。
    #[must_use]
    pub fn will_write(&self) -> bool {
        self.changes_value || self.before_type != self.after_type
    }
}

/// 按票据规定的规则决定写回时的注册表类型。
///
/// **值含 `%` 才用 `REG_EXPAND_SZ`；否则保留原有类型；再否则 `REG_SZ`。**
///
/// 本机实测就是这个规则在救场：机器级 `Path` 标了 `REG_EXPAND_SZ` 却**零变量**，
/// 用户级是 `REG_SZ` —— 无条件写 `REG_EXPAND_SZ` 会让"值里没有变量"这件事
/// 与类型不符（进而让下一次读取的展开行为改变）；无条件写 `REG_SZ` 则会把
/// 用户值里的 `%USERPROFILE%` **永久冻成字面量**。
#[must_use]
pub fn write_type_for(value: &str, original: Option<RegType>) -> RegType {
    if value.contains('%') {
        RegType::ExpandSz
    } else if let Some(original) = original {
        original
    } else {
        RegType::Sz
    }
}

fn user_scope(block: &impl EnvBlock) -> (String, RegType) {
    match block.get(EnvScope::User, PATH_NAME) {
        Some(var) => (var.value_raw, var.reg_type),
        None => (String::new(), RegType::Sz),
    }
}

/// 计划把 `dir` 追加到用户级 `PATH`。
///
/// **只写用户级。** 机器级要提权，而我们不做"顺手提权"这件事（见设计决策）。
///
/// **追加在末尾，所以加目录永远不会遮蔽我们自己的 shim** —— 已经在前面的条目
/// 优先级不变，而我们（以及任何已有的条目）都还在前面。所以这个函数**不需要**
/// `shim_dir`：只有删除才需要保护它。
///
/// # Errors
///
/// `dir` 里含 `;` 时返回 [`PlatformError::Unsupported`]：`PATH` 用 `;` 分隔，
/// **引号不能保护它**，所以 `C:\a;b` 会被写成两条条目 —— 那不是用户想要的事，
/// 而一旦写进去，我们也分不出原来是一条还是两条。宁可当场拒绝。
pub fn plan_add(block: &impl EnvBlock, dir: &str) -> Result<PathPlan, PlatformError> {
    reject_separator_in_argument(dir)?;
    let (before_raw, before_type) = user_scope(block);
    let entries = parse_entries(&before_raw);
    let key = normalize_entry(dir.trim().trim_matches('"')).to_lowercase();
    let mut changes = Vec::new();
    let mut after: Vec<String> = entries
        .iter()
        .filter(|e| !e.is_empty())
        .map(|e| e.raw.trim().to_owned())
        .collect();
    let dropped = entries.iter().filter(|e| e.is_empty()).count();

    if key.is_empty() {
        changes.push(PathChange::Noop {
            value: dir.to_owned(),
            reason: NoopReason::Absent,
        });
    } else if let Some(at) = after
        .iter()
        .position(|e| normalize_entry(e).to_lowercase() == key)
    {
        changes.push(PathChange::Noop {
            value: after[at].clone(),
            reason: NoopReason::AlreadyPresent,
        });
    } else {
        changes.push(PathChange::Add {
            value: dir.to_owned(),
            at: after.len(),
        });
        after.push(dir.to_owned());
    }
    if dropped > 0 {
        changes.push(PathChange::DropEmptySegments { count: dropped });
    }

    Ok(finish_plan(
        before_raw,
        before_type,
        after,
        machine_chars(block),
        changes,
    ))
}

/// 拒绝会把一条条目劈成两条的输入。见 [`plan_add`] 的 `# Errors`。
fn reject_separator_in_argument(dir: &str) -> Result<(), PlatformError> {
    if dir.contains(';') {
        return Err(PlatformError::Unsupported {
            what: format!("含 `;` 的目录名（`{dir}`）—— 它会被写成两条 PATH 条目"),
        });
    }
    Ok(())
}

/// 机器级 `Path` 的字符数。**只用来算预算**，从不写它。
fn machine_chars(block: &impl EnvBlock) -> usize {
    block
        .get(EnvScope::Machine, PATH_NAME)
        .map_or(0, |var| var.value_raw.chars().count())
}

/// 计划从用户级 `PATH` 里删掉 `dir` 的**全部**出现。
///
/// `shim_dir` 是保护：拿它当参数删时**什么都不做**（见
/// [`NoopReason::ProtectedShimDir`]）—— 那是把刚装好的命令从 `PATH` 上摘掉。
///
/// # Errors
///
/// 同 [`plan_add`]。
pub fn plan_remove(
    block: &impl EnvBlock,
    dir: &str,
    shim_dir: Option<&Path>,
) -> Result<PathPlan, PlatformError> {
    reject_separator_in_argument(dir)?;
    let (before_raw, before_type) = user_scope(block);
    let entries = parse_entries(&before_raw);
    let key = normalize_entry(dir.trim().trim_matches('"')).to_lowercase();
    let shim_key = shim_dir.map(|d| normalize_entry(&d.to_string_lossy()).to_lowercase());
    let machine = machine_chars(block);
    let mut changes = Vec::new();

    if shim_key.as_deref() == Some(key.as_str()) {
        // 删掉自己的 shim 目录 = 把刚装好的命令从 PATH 上摘掉。
        changes.push(PathChange::Noop {
            value: dir.to_owned(),
            reason: NoopReason::ProtectedShimDir,
        });
        return Ok(finish_plan(
            before_raw,
            before_type,
            entries
                .iter()
                .filter(|e| !e.is_empty())
                .map(|e| e.raw.trim().to_owned())
                .collect(),
            machine,
            changes,
        ));
    }

    let mut after = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if entry.is_empty() {
            continue;
        }
        let value = entry.raw.trim().to_owned();
        if !key.is_empty() && normalize_entry(&value).to_lowercase() == key {
            changes.push(PathChange::Remove {
                value,
                was_at: index,
            });
        } else {
            after.push(value);
        }
    }
    if !changes
        .iter()
        .any(|c| matches!(c, PathChange::Remove { .. }))
    {
        changes.push(PathChange::Noop {
            value: dir.to_owned(),
            reason: NoopReason::Absent,
        });
    }
    let dropped = entries.iter().filter(|e| e.is_empty()).count();
    if dropped > 0 {
        changes.push(PathChange::DropEmptySegments { count: dropped });
    }

    Ok(finish_plan(
        before_raw,
        before_type,
        after,
        machine,
        changes,
    ))
}

fn finish_plan(
    before_raw: String,
    before_type: RegType,
    after: Vec<String>,
    machine_chars: usize,
    mut changes: Vec<PathChange>,
) -> PathPlan {
    let after_raw = after.join(";");
    let after_type = write_type_for(&after_raw, Some(before_type));
    let changes_value = after_raw != before_raw;
    if before_type != after_type {
        changes.push(PathChange::Retype {
            from: before_type,
            to: after_type,
        });
    }
    // `PathBudget::of(effective, registry)` 要两个数，而计划里只算得出注册表那一半。
    // 于是两个都传注册表口径，并把这件事写在 `budget_after` 的文档里 ——
    // **它是下界**：真实的生效长度只会更长（本机多 78 字符的 MSIX 注入）。
    let registry_after = machine_chars + after_raw.chars().count();
    PathPlan {
        scope: EnvScope::User,
        before_raw,
        after_raw,
        before_type,
        after_type,
        changes,
        changes_value,
        budget_after: PathBudget::of(registry_after, registry_after),
    }
}

/// 落盘结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathApplied {
    /// 真的写了吗（计划和现状一致时是 `false`）。
    pub wrote: bool,
    /// 写进去的类型。
    pub reg_type: RegType,
    /// 写进去的字符数。
    pub chars: usize,
    /// 广播回执：收到了几个顶层窗口的应答。
    /// **它只是"广播发出去了"的证据，不是"所有进程都更新了"的证据** ——
    /// 已经跑着的进程拿不到新环境（环境块是 `CreateProcess` 时复制的）。
    pub broadcast_replies: usize,
}

/// 把计划写进 `HKCU\Environment`，然后广播 `WM_SETTINGCHANGE`。
///
/// **一次写完整条值**，不是逐段改 —— 逐段改会有中间状态，而中间状态就是
/// 「`PATH` 暂时少了几个目录」。
///
/// # Errors
///
/// 注册表写失败（权限、磁盘）时返回 [`PlatformError`]。
pub fn apply<R: Registry>(
    registry: &R,
    plan: &PathPlan,
    broadcast: impl FnOnce() -> usize,
) -> Result<PathApplied, PlatformError> {
    if !plan.will_write() {
        return Ok(PathApplied {
            wrote: false,
            reg_type: plan.before_type,
            chars: plan.before_raw.chars().count(),
            broadcast_replies: 0,
        });
    }
    let value = match plan.after_type {
        RegType::Sz => RegValue::Sz(plan.after_raw.clone()),
        RegType::ExpandSz => RegValue::ExpandSz(plan.after_raw.clone()),
    };
    registry.set_value(RegHive::Hkcu, USER_ENV_SUBKEY, PATH_NAME, &value)?;
    let broadcast_replies = broadcast();
    Ok(PathApplied {
        wrote: true,
        reg_type: plan.after_type,
        chars: plan.after_raw.chars().count(),
        broadcast_replies,
    })
}

/// 一个方便的组合：分析 + 计划 + （可选）落盘。
///
/// `dry_run` 时走的是**同一条**计划代码路径，只是不调 [`apply`] ——
/// 这正是票据要的"`--dry-run` 走同一套代码路径"。
///
/// # Errors
///
/// 同 [`apply`]。
pub fn run<E, R>(
    block: &E,
    registry: &R,
    action: PathAction<'_>,
    dry_run: bool,
    broadcast: impl FnOnce() -> usize,
) -> Result<(PathPlan, Option<PathApplied>), PlatformError>
where
    E: EnvBlock,
    R: Registry,
{
    let plan = match action {
        PathAction::Add { dir } => plan_add(block, dir)?,
        PathAction::Remove { dir, shim_dir } => plan_remove(block, dir, shim_dir)?,
    };
    if dry_run {
        return Ok((plan, None));
    }
    let applied = apply(registry, &plan, broadcast)?;
    Ok((plan, Some(applied)))
}

/// 要对用户级 `PATH` 做的事。
#[derive(Debug, Clone, Copy)]
pub enum PathAction<'a> {
    Add {
        dir: &'a str,
    },
    Remove {
        dir: &'a str,
        shim_dir: Option<&'a Path>,
    },
}

/// 用户级 `PATH` 里应该放但我们还没放的那些目录（预留给 `tuoen doctor`）。
#[must_use]
pub fn missing_from_path<'a>(analysis: &PathAnalysis, wanted: &'a [PathBuf]) -> Vec<&'a PathBuf> {
    let present: Vec<String> = analysis
        .scopes
        .iter()
        .flat_map(|s| s.entries.iter().map(PathEntry::key))
        .collect();
    wanted
        .iter()
        .filter(|dir| {
            let key = normalize_entry(&dir.to_string_lossy()).to_lowercase();
            !present.contains(&key)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_keep_their_raw_form_and_lose_only_comparison_noise() {
        let entries = parse_entries(r#"C:\a;  D:\b  ;"C:\program files\x";;;C:\root\"#);
        assert_eq!(entries.len(), 6);
        assert_eq!(entries[0].value, r"C:\a");
        assert_eq!(entries[1].value, r"D:\b");
        assert!(!entries[1].quoted);
        assert_eq!(entries[2].value, r"C:\program files\x");
        assert!(entries[2].quoted);
        // 原文一个字都不许动 —— 写回时按原文写。
        assert_eq!(entries[1].raw, "  D:\\b  ");
        assert!(entries[3].is_empty() && entries[4].is_empty());
        // 结尾的反斜杠只是在**比较形态**里去掉，原文仍然带着它。
        assert_eq!(entries[5].value, r"C:\root");
        assert_eq!(entries[5].raw, r"C:\root\");
        // `C:\` 的那一个反斜杠是根，不能吃掉。
        assert_eq!(normalize_entry(r"C:\"), r"C:\");
        assert_eq!(normalize_entry(r"C:\a\b\"), r"C:\a\b");
    }

    #[test]
    fn a_trailing_semicolon_is_an_empty_entry_not_a_lost_one() {
        let entries = parse_entries("C:\\a;");
        assert_eq!(entries.len(), 2);
        assert!(entries[1].is_empty());
    }

    #[test]
    fn hardcoded_usernames_are_found_but_variables_are_not() {
        assert_eq!(
            hardcoded_username(r"C:\Users\Muelsyse\AppData\Local\nvm").as_deref(),
            Some("Muelsyse")
        );
        assert_eq!(
            hardcoded_username(r"C:\Users\Muelsyse"),
            Some("Muelsyse".to_owned())
        );
        // 可移植的两条路：变量引用与不含 \Users\ 的路径。
        assert_eq!(hardcoded_username(r"%USERPROFILE%\AppData\Local\nvm"), None);
        assert_eq!(hardcoded_username(r"C:\Users\%USERNAME%\x"), None);
        assert_eq!(hardcoded_username(r"C:\Program Files\Java"), None);
        assert_eq!(hardcoded_username(r"C:\Users\"), None);
    }

    #[test]
    fn the_write_type_rule_is_the_one_the_ticket_names() {
        // 值含 `%` → 必须 EXPAND，否则变量被永久冻成字面量。
        assert_eq!(
            write_type_for(r"C:\a;%USERPROFILE%\b", Some(RegType::Sz)),
            RegType::ExpandSz
        );
        // 不含 `%` → 保留原类型（本机机器级 Path 是 EXPAND_SZ 却零变量）。
        assert_eq!(
            write_type_for(r"C:\a", Some(RegType::ExpandSz)),
            RegType::ExpandSz
        );
        assert_eq!(write_type_for(r"C:\a", Some(RegType::Sz)), RegType::Sz);
        // 没有原类型 → SZ。
        assert_eq!(write_type_for(r"C:\a", None), RegType::Sz);
    }

    #[test]
    fn the_budget_is_read_from_the_effective_path_not_from_one_scope() {
        // 本机实测的形状：注册表 1703，进程 1781（多出的是 MSIX 别名注入）。
        let budget = PathBudget::of(1781, 1703);
        assert_eq!(budget.effective_chars, 1781);
        assert_eq!(budget.registry_chars, 1703);
        assert_eq!(budget.level, BudgetLevel::Ok);
        assert_eq!(budget.remaining, PATH_CLIFF_CMD - 1781);
    }

    #[test]
    fn the_two_cliffs_are_not_the_same_number() {
        // 75% / 90% / 超过 8191 三档，边界都要钉住。
        assert_eq!(PathBudget::of(6143, 0).level, BudgetLevel::Ok);
        assert_eq!(PathBudget::of(6144, 0).level, BudgetLevel::Warning);
        assert_eq!(PathBudget::of(7371, 0).level, BudgetLevel::Warning);
        assert_eq!(PathBudget::of(7372, 0).level, BudgetLevel::Critical);
        // **8191 本身还没越界**：`cmd.exe` 是在"超过 8191"时才整条忽略 `PATH` 的，
        // 所以 8191 是最后一个可用长度，此时是 Critical 而不是 Exceeded。
        assert_eq!(PathBudget::of(8191, 0).level, BudgetLevel::Critical);
        assert_eq!(PathBudget::of(8192, 0).level, BudgetLevel::Exceeded);
        // 超过之后 remaining 不做回绕。
        assert_eq!(PathBudget::of(9000, 0).remaining, 0);
        assert!(PathBudget::of(8192, 0).level.is_alarming());
    }
}
