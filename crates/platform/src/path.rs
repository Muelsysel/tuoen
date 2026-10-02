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
    E: EnvBlock + ?Sized,
    P: ProcessEnv + ?Sized,
    F: FileSystem + ?Sized,
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
///
/// # 为什么是 `pub`
///
/// `tuoen doctor` 的 `path.shadowed` 要问的正是这个问题，而**"谁赢了名字冲突"
/// 的判据只能有一处**：`path add` 用它决定要不要提醒，`doctor` 用它报体检结论。
/// 抄一份到 core 里迟早会与这里漂移，而漂移的后果是两处对同一台机器给出不同的
/// 答案（"谁遮蔽了谁"恰好是最容易被抄错的那类逻辑：扩展名集合、只列一次目录、
/// 空目录的分母）。
#[must_use]
pub fn detect_shadowing<F: FileSystem + ?Sized>(
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
pub fn shim_commands<F: FileSystem + ?Sized>(fs: &F, shim_dir: &Path) -> Vec<String> {
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
        /// 追加到第几段。**这是「新列表下标」**（空条目已经被丢掉），
        /// 与 [`Self::Remove::was_at`] 的"含空条目的原文下标"**不可比** ——
        /// 同一个约定见 [`Self::Move`] 的两个字段。
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
    /// 一条条目换了位置（值没变）。
    Move {
        /// 新列表里这一格的值。
        value: String,
        /// 原来在第几段。**这是「含空条目的原文下标」**（与 `Remove.was_at`
        /// 同款，即 [`ScopedPath::entries`] 的下标），而 `at` 是**新列表的下标**
        /// （空条目已经被丢掉）。**两个字段不可比**：`A;;A;B` → `A;B` 时 B 是
        /// `was_at: 3, at: 1`。
        was_at: usize,
        /// 现在在第几段。**新列表下标，不含空条目** —— 见 `was_at` 的说明。
        at: usize,
    },
    /// 一条条目的值被改写（位置没变）。
    ///
    /// **我们不判断这次改写是不是"同一个目录"** —— 那是一个没有判据的判断
    /// （`C:\Dev\jdk-17` 与 `C:\Dev\doxygen` 像不像？谁都答不了）。这里只报告
    /// "这一段的值变了"：同下标 + 值不同就是改写，别的都交给 [`PathChange::Add`] /
    /// [`PathChange::Remove`]（它们只留给"只在一侧出现"的条目）。
    Replace {
        /// 改写后的值。
        value: String,
        /// 改写前的值。**`trim()` 之后的原文形态**（与 `Remove.value` 同款），
        /// 不是 [`PathEntry::raw`] 那种带首尾空白与引号的逐字节原文。
        was: String,
        /// 在第几段。**新列表下标**（空条目已经被丢掉），两边同一个下标。
        at: usize,
    },
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

/// 拒绝会把**整条 `PATH`** 的某一条劈成两条的输入。见 [`plan_rewrite`] 的 `# Errors`。
///
/// 与 [`reject_separator_in_argument`] 的立场完全一样（`;` 是分隔符，引号保护不了它），
/// 只是这里逐条检查整个列表 —— 重建的产物是完整列表，坏一条就等于坏一整条 `PATH`。
fn reject_separator_in_entries(entries: &[String]) -> Result<(), PlatformError> {
    for entry in entries {
        if entry.contains(';') {
            return Err(PlatformError::Unsupported {
                what: format!("含 `;` 的目录名（`{entry}`）—— 它会被写成两条 PATH 条目"),
            });
        }
    }
    Ok(())
}

/// 整值重写计划：把**用户级** `PATH` 换成一份完整的新条目列表。
///
/// 与 [`plan_add`] / [`plan_remove`] 的区别：那两个是"加一条/删一条"，
/// 这个是"这整条值以后长这样" —— 重建（diff + rebuild）的产物是完整列表，
/// 所以写回也必须是整值替换（逐段改会有中间状态，而中间状态就是
/// 「`PATH` 暂时少了几个目录」）。
///
/// **只接受 [`EnvScope::User`]**：机器级要提权，而 tuoen 不做静默提权
/// （决策 12/136）。机器级传进来返回 [`PlatformError::Unsupported`]。
///
/// # 预算口径
///
/// [`PathPlan::budget_after`] 按**注册表口径**算：机器级 `Path` 原文（从同一个
/// `block` 读，取不到当 0）**加上**重建后的用户级原文。两个字段传同一个数。
///
/// **它是下界**：不含**进程注入项**（本机 78 字符，PowerShell 的 MSIX 别名），
/// 所以真实的生效长度只会比它更长、只会比它更早撞 8191 悬崖。
///
/// 与 `finish_plan`（[`plan_add`] / [`plan_remove`] 走它）**同一个口径** ——
/// 同一个字段名必须只有一种语义，否则 CLI 把两条路径的数字并排印给用户看时，
/// 那两个"长度"根本不可比。
///
/// # Errors
///
/// `scope != User`，或 `after_entries` 里有一条含 `;`（它是分隔符，引号保护不了它）。
pub fn plan_rewrite(
    block: &impl EnvBlock,
    scope: EnvScope,
    after_entries: &[String],
) -> Result<PathPlan, PlatformError> {
    if scope != EnvScope::User {
        return Err(PlatformError::Unsupported {
            what: format!(
                "`{}` 作用域的整值重写 —— 机器级要提权，tuoen 不做静默提权（决策 12/136）",
                scope.as_str()
            ),
        });
    }
    reject_separator_in_entries(after_entries)?;

    // 与 `user_scope` 同一套读法：没有这个变量就是空串 + `REG_SZ`。
    let (before_raw, before_type) = user_scope(block);

    // 空条目不是目录（决策 130）：`trim()` 之后为空的一律丢掉，
    // 它既不该成为新列表里的一条，也不该参与比对。
    let after: Vec<String> = after_entries
        .iter()
        .map(|entry| entry.trim().to_owned())
        .filter(|entry| !entry.is_empty())
        .collect();
    let after_raw = after.join(";");
    let after_type = write_type_for(&after_raw, Some(before_type));
    let changes_value = before_raw != after_raw;

    // ── 逐条比对 ────────────────────────────────────────────────────
    //
    // 一条条目"没动"的定义就是它还在同一格上；同下标 + 字符串不同 = `Replace`。
    //
    // **`before_raw` 为空串时没有任何条目**：`parse_entries("")` 会给出**一个**
    // 空条目（`"".split(';')` 的机械结果），但"用户级 `Path` 根本不存在"与
    // "用户级 `Path` 是空串"这两种形状里都没有段可谈，那个空条目只是切分产物。
    // 保留它就会让"把一条本来就空的 `PATH` 清空"报出 `DropEmptySegments { count: 1 }`
    // —— 那是噪声，不是发现。
    let before = if before_raw.is_empty() {
        Vec::new()
    } else {
        parse_entries(&before_raw)
    };
    let empty_before: Vec<usize> = before
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.is_empty())
        .map(|(index, _)| index)
        .collect();

    // 目标列表里"这个值出现在哪"。
    let after_slots: Vec<(String, usize)> = after
        .iter()
        .enumerate()
        .map(|(at, value)| (normalize_entry(value).to_lowercase(), at))
        .collect();
    let slot_of = |key: &str| {
        after_slots
            .iter()
            .find(|(slot, _)| slot == key)
            .map(|(_, at)| *at)
    };

    // `changes` 的骨架按 after 的下标摆好，逐格填一个变更 —— 这样"顺序 =
    // 新列表的形状"是**结构性的**，不是靠拼装顺序碰巧对。
    let mut changes: Vec<Option<PathChange>> = vec![None; after.len()];
    let mut removed: Vec<(usize, PathChange)> = Vec::new();
    // **两套坐标分开记，绝不混用**（这个模块里最容易写错的地方）：
    // - `paired_after`：新列表的第几格已经有结论了（`after` 的下标）。第一、
    //   二遍往里放，第三遍只处理**不在**里面的格子。
    // - `paired_before`：原文的第几段处理过了（`before` 的下标，**含空条目**）。
    // - `moved_before`：第二遍按值认走的那些**原文**下标 —— 第三遍不许再翻案。
    // 拿其中一套去问另一套的下标，在原文有空条目时必然错位（这个 bug 写过两次）。
    let mut paired_after: Vec<usize> = Vec::new();
    let mut paired_before: Vec<usize> = Vec::new();
    let mut moved_before: Vec<usize> = Vec::new();

    // ── 第一遍：**同下标**配对 ───────────────────────────────────────
    //
    // 一条条目"没动"的定义就是它还在同一格上。大小写折叠后相同 → 值没变
    // （`Noop`）或被改写（`Replace`，**不判断这次改写是不是"同一个目录"**：
    // 同下标 + 字符串不同就是改写 —— 这是"没有判据的判断"里唯一站得住的那条）。
    for (index, entry) in before.iter().enumerate() {
        let Some(other) = after.get(index) else {
            break;
        };
        if entry.is_empty() {
            // 空条目不是目录（决策 130）：它不与任何一格配对，也不产生 `Remove`；
            // 它只贡献下面那个 `DropEmptySegments`。
            continue;
        }
        if entry.key() != after_slots[index].0 {
            continue;
        }
        paired_before.push(index);
        paired_after.push(index);
        let was = entry.raw.trim().to_owned();
        changes[index] = Some(if *other == was {
            PathChange::Noop {
                value: other.clone(),
                reason: NoopReason::AlreadyPresent,
            }
        } else {
            PathChange::Replace {
                value: other.clone(),
                was,
                at: index,
            }
        });
    }

    // ── 第二遍：按**值**配对（大小写折叠后相同）→ `Move` / `Remove` ──────
    //
    // 走到这里的条目，同下标上要么没有格子、要么值不一样。三种去向：
    // 值在目标里出现而那一格还空着 → `Move`；目标里根本没有这个值 → `Remove`；
    // 值在目标里有但那一格被**更早的相同值**占了 → 这一次出现是重复，也归
    // `Remove`（首次出现赢，决策 129）。
    for (index, entry) in before.iter().enumerate() {
        if entry.is_empty() || paired_before.contains(&index) {
            continue;
        }
        let was = entry.raw.trim().to_owned();
        match slot_of(&entry.key()) {
            Some(at) if !paired_after.contains(&at) => {
                paired_after.push(at);
                paired_before.push(index);
                moved_before.push(index);
                changes[at] = Some(PathChange::Move {
                    value: after[at].clone(),
                    was_at: index,
                    at,
                });
            }
            // 这个值在目标里**根本没有** → 删掉。第三遍若判定"同下标上是被
            // 换成了别的值"，会撤掉这一条（见第三遍的 `retain`）。
            None => {
                removed.push((
                    index,
                    PathChange::Remove {
                        value: was,
                        was_at: index,
                    },
                ));
            }
            // 这个值在目标里有、但那一格被**更早的相同值**占了 → 本次这次出现
            // 是重复（首次出现赢，决策 129）→ 删掉。
            //
            // **必须报 `Remove`**：`Remove` 是"这个值不见了"的唯一说法，而
            // "值不见了却没报"是这份变更清单最坏的一种错 —— 用户读到的是一份
            // 漏了删除的计划。例外只有一个：重复的那一次恰好落在**它同下标的
            // 那一格**上、而那一格还没有结论 —— 那时第三遍把它报成 `Replace`
            // 并撤掉这一条（`Replace.was` 同样交代了那个旧值的去向）。
            Some(_) => {
                removed.push((
                    index,
                    PathChange::Remove {
                        value: was,
                        was_at: index,
                    },
                ));
            }
        }
    }

    // ── 第三遍：剩下的同下标配对 → `Replace` ─────────────────────────
    //
    // 前两遍都放下的，是"这一格上原来有东西、现在有**另外一个**东西"。那条旧的
    // 必须与目标里任何值都不相同，否则第二遍就会把它当 `Move` 认走 —— 这正是
    // "换位"与"改写"的分界：换位时双方的值都在目标里，各归各的 `Move`。
    for at in 0..before.len() {
        // 这一格在新列表里**不存在** → 它没有被改写，只是在原文里没了
        // （第二遍已经报过 `Remove`）。注意这里**必须**遍历原文的下标：
        // 新列表只覆盖到它自己的长度，重复的条目可能落在它之外。
        if at >= after.len() {
            continue;
        }
        // 判据是"**这一格有没有结论**"，不是"下标有没有被记进某张表" ——
        // 第二遍的拒绝分支也在表格里，用它当判据会把这三种情况混成一种。
        if changes[at].is_some() {
            continue;
        }
        let Some(entry) = before.get(at) else {
            continue;
        };
        if entry.is_empty() {
            continue;
        }
        let was = entry.raw.trim().to_owned();
        paired_after.push(at);
        paired_before.push(at);
        // 第二遍为这一格产生过 `Remove`（那个值在目标里根本不存在）——
        // 现在它被判定为"同下标上换成了别的值"，那条 `Remove` 必须撤掉，
        // 否则同一个下标会同时出现"删掉"与"改写"两条结论。
        removed.retain(|(was_at, _)| *was_at != at);
        changes[at] = Some(PathChange::Replace {
            value: after[at].clone(),
            was,
            at,
        });
    }

    // 顺序是契约：空段在前，然后按 after 的下标顺序逐条，最后按 before 的
    // 下标顺序追加删除。
    let mut ordered = Vec::new();
    if !empty_before.is_empty() {
        ordered.push(PathChange::DropEmptySegments {
            count: empty_before.len(),
        });
    }
    ordered.extend(changes.into_iter().flatten());
    removed.sort_by_key(|(was_at, _)| *was_at);
    ordered.extend(removed.into_iter().map(|(_, change)| change));

    // 预算的两个数都传"机器级 + 用户级重建后"的注册表口径 —— 见上面
    // `# 预算口径`：它不含进程注入项，是**下界**。与 `finish_plan` 同一口径。
    let registry_after = machine_chars(block) + after_raw.chars().count();
    Ok(PathPlan {
        scope: EnvScope::User,
        before_raw,
        after_raw,
        before_type,
        after_type,
        changes: ordered,
        changes_value,
        budget_after: PathBudget::of(registry_after, registry_after),
    })
}

/// 把整值重写计划写进 `HKCU\Environment`，然后广播 `WM_SETTINGCHANGE`。
///
/// 与 [`apply`] 的关系：写路径**完全复用** [`apply`]（它本来就是整值写 + 广播，
/// 决策 137），这里多出来的唯一一件事是**拒绝非用户级** —— 让"机器级只能提权改"
/// 这件事在类型层面无法绕过，而不是靠调用方自觉。
///
/// # Errors
///
/// 同 [`apply`]，外加 `plan.scope != EnvScope::User` 时的 [`PlatformError::Unsupported`]。
pub fn apply_rewrite<R: Registry>(
    registry: &R,
    plan: &PathPlan,
    broadcast: impl FnOnce() -> usize,
) -> Result<PathApplied, PlatformError> {
    if plan.scope != EnvScope::User {
        return Err(PlatformError::Unsupported {
            what: format!(
                "`{}` 作用域的整值重写 —— 机器级要提权，tuoen 不做静默提权（决策 12/136）",
                plan.scope.as_str()
            ),
        });
    }
    apply(registry, plan, broadcast)
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
    use std::cell::Cell;

    use crate::env_block::{InMemoryEnv, RealEnvBlock};
    use crate::fixture::{FakeFileSystem, FakeRegistry, FixtureKey, MachineFixture};

    // ── 整值重写（`plan_rewrite` / `apply_rewrite`）─────────────────────────
    //
    // 这些用例在**零真实状态**上跑：假注册表 + 假文件系统 + 内存环境块。
    // 它们要证明的不是"重建算得对"（那是 `crates/core/src/pathdiff.rs` 的事），
    // 而是"**这份完整列表被整值写回了**，且写回路径与 `apply` 是同一条"。

    /// 一份带用户级 `Path` 的假注册表，以及读它的 `EnvBlock`。
    ///
    /// 为什么用 `RealEnvBlock` 而不是 `InMemoryEnv`：`InMemoryEnv` 实现的是
    /// `ProcessEnv`（进程环境块），**不是** `EnvBlock` —— 它答不了"注册表里这条是什么"。
    /// 测试要的恰好是"改之前从注册表读到什么"，所以这里必须走注册表那条路。
    ///
    /// 注册表 `clone()` 出来的是**同一个**注册表（`FakeRegistry` 内部是 `Rc<RefCell>`），
    /// 所以"从重读的那一份里看到刚写的值"这件事成立。
    type TestBlock = RealEnvBlock<FakeRegistry, FakeFileSystem>;

    fn user_path(env_value: &str, kind: RegType) -> (TestBlock, InMemoryEnv, FakeRegistry) {
        let value = match kind {
            RegType::Sz => RegValue::Sz(env_value.to_owned()),
            RegType::ExpandSz => RegValue::ExpandSz(env_value.to_owned()),
        };
        let fixture = MachineFixture {
            env: BTreeMap::from([(PATH_NAME.to_owned(), env_value.to_owned())]),
            registry: vec![FixtureKey::new(
                RegHive::Hkcu,
                USER_ENV_SUBKEY,
                BTreeMap::from([(PATH_NAME.to_owned(), value)]),
            )],
            ..MachineFixture::default()
        };
        let built = fixture.build();
        let block = RealEnvBlock::new(built.registry.clone(), built.fs.clone());
        (block, built.env, built.registry)
    }

    /// 同上，外加一条**机器级** `Path`（预算口径要看得见它）。
    fn user_path_with_machine(
        env_value: &str,
        machine_value: &str,
        kind: RegType,
    ) -> (TestBlock, InMemoryEnv, FakeRegistry) {
        let value = match kind {
            RegType::Sz => RegValue::Sz(env_value.to_owned()),
            RegType::ExpandSz => RegValue::ExpandSz(env_value.to_owned()),
        };
        let fixture = MachineFixture {
            env: BTreeMap::from([(PATH_NAME.to_owned(), env_value.to_owned())]),
            registry: vec![
                FixtureKey::new(
                    RegHive::Hkcu,
                    USER_ENV_SUBKEY,
                    BTreeMap::from([(PATH_NAME.to_owned(), value)]),
                ),
                FixtureKey::new(
                    RegHive::Hklm,
                    crate::env_block::MACHINE_ENV_SUBKEY,
                    BTreeMap::from([(
                        PATH_NAME.to_owned(),
                        RegValue::ExpandSz(machine_value.to_owned()),
                    )]),
                ),
            ],
            ..MachineFixture::default()
        };
        let built = fixture.build();
        let block = RealEnvBlock::new(built.registry.clone(), built.fs.clone());
        (block, built.env, built.registry)
    }

    fn entries(list: &[&str]) -> Vec<String> {
        list.iter().map(|entry| (*entry).to_owned()).collect()
    }

    /// 写完之后的注册表里那条值。
    fn reread(registry: &FakeRegistry) -> RegValue {
        registry
            .value(RegHive::Hkcu, USER_ENV_SUBKEY, PATH_NAME)
            .expect("注册表里应当有一条用户级 Path")
    }

    #[test]
    fn rewrite_reports_a_move_when_an_entry_changed_place() {
        // `A;B;C` → `A;C;B`：C 从第 2 段挪到第 1 段。
        let (block, _process, _registry) = user_path(r"A;B;C", RegType::Sz);
        let plan =
            plan_rewrite(&block, EnvScope::User, &entries(&["A", "C", "B"])).expect("用户级");
        assert_eq!(plan.after_raw, r"A;C;B");
        assert!(plan.changes_value);
        assert_eq!(plan.scope, EnvScope::User);
        assert!(
            plan.changes.contains(&PathChange::Move {
                value: "C".to_owned(),
                was_at: 2,
                at: 1,
            }),
            "C 换了位置就报 Move：{:?}",
            plan.changes
        );
        // 顺带钉住"B 也挪了" —— 这一条票面没有要求，但它是同一套判据的另一半，
        // 不写下来的话"只报了 C"与"判据漏了一半"在输出里长得一样。
        assert!(
            plan.changes.contains(&PathChange::Move {
                value: "B".to_owned(),
                was_at: 1,
                at: 2,
            }),
            "B 同样换了位置：{:?}",
            plan.changes
        );
        assert!(
            plan.changes.iter().all(|change| !matches!(
                change,
                PathChange::Add { .. } | PathChange::Remove { .. }
            )),
            "A;C;B 是同一批值的重排，不该出现增删：{:?}",
            plan.changes
        );
        // A 在**同一个下标**上原样不动 —— 它是 `Noop`，不是 `Move`。
        // 这两类的分界是"值出现在哪一格"，而不是"中间有没有人被动过"。
        assert!(
            plan.changes.contains(&PathChange::Noop {
                value: "A".to_owned(),
                reason: NoopReason::AlreadyPresent,
            }),
            "A 原位不动就是 Noop：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_reports_a_replace_when_only_the_value_changed_in_place() {
        // 位置没动，字符串变了 —— 大小写重写与用户名重写都长这样。
        let (block, _process, _registry) = user_path(r"C:\Users\old\bin;C:\x", RegType::Sz);
        let plan = plan_rewrite(
            &block,
            EnvScope::User,
            &entries(&[r"C:\Users\new\bin", r"C:\x"]),
        )
        .expect("用户级");
        assert_eq!(plan.after_raw, r"C:\Users\new\bin;C:\x");
        assert!(
            plan.changes.contains(&PathChange::Replace {
                value: r"C:\Users\new\bin".to_owned(),
                was: r"C:\Users\old\bin".to_owned(),
                at: 0,
            }),
            "换的是值、不是位置：{:?}",
            plan.changes
        );
        // `Replace` 必须是一个**独立**的类，不能退化成 add + remove。
        assert!(
            plan.changes.iter().all(|change| !matches!(
                change,
                PathChange::Move { .. } | PathChange::Add { .. } | PathChange::Remove { .. }
            )),
            "第二条原样、第一条只是被改写，不该出现 Move/Add/Remove：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_drops_empty_segments_dedupes_and_keeps_order() {
        // `A;;A;B` → `A;B`：清掉 1 个空段、B 从原文第 3 段挪到新列表第 1 段、
        // 第一次出现的 A 原地不动、**第二次出现的 A（原文第 2 段）归 `Remove`**
        // —— "首次出现赢"（决策 129）：新列表第 0 格已经被第一次的 A 占了，
        // 那一次出现就没有位置。
        let (block, _process, _registry) = user_path(r"A;;A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "B"])).expect("用户级");
        assert_eq!(plan.after_raw, r"A;B");
        // 空段在前，这是 `changes` 的读法契约。
        assert_eq!(
            plan.changes.first(),
            Some(&PathChange::DropEmptySegments { count: 1 }),
            "空段变更排在最前：{:?}",
            plan.changes
        );
        assert!(
            plan.changes.iter().any(
                |change| matches!(change, PathChange::Noop { value, reason: NoopReason::AlreadyPresent } if value == "A")
            ),
            "第一次出现的 A 原地不动：{:?}",
            plan.changes
        );
        // `at` 是**新列表**下标（空段已经不在了），`was_at` 是**原文**里的下标
        // —— 两套坐标不一致是这里的固有形状，`Move` 的两个字段各属一套。
        assert!(
            plan.changes.contains(&PathChange::Move {
                value: "B".to_owned(),
                was_at: 3,
                at: 1,
            }),
            "B 从原文第 3 段挪到新列表第 1 段：{:?}",
            plan.changes
        );
        assert!(
            plan.changes.contains(&PathChange::Remove {
                value: "A".to_owned(),
                was_at: 2,
            }),
            "第二次出现的 A 归 Remove：{:?}",
            plan.changes
        );
        assert!(
            plan.changes
                .iter()
                .all(|change| !matches!(change, PathChange::Add { .. })),
            "这批值的重排不该出现 Add：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_reports_a_duplicate_as_remove_when_its_slot_is_untouched() {
        // `A;B;A` → `A;B`：A 与 B 都原地不动，多出来的那一次 A 落在**别人的**
        // 格子上（新列表第 0 格已经被第一次的 A 占了）→ 归 `Remove`，
        // 报的是它在**原文**里的下标 2。
        let (block, _process, _registry) = user_path(r"A;B;A", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "B"])).expect("用户级");
        assert_eq!(plan.after_raw, r"A;B");
        assert!(
            plan.changes.contains(&PathChange::Remove {
                value: "A".to_owned(),
                was_at: 2,
            }),
            "第二次出现的 A 归 Remove：{:?}",
            plan.changes
        );
        assert_eq!(
            plan.changes
                .iter()
                .filter(|change| matches!(change, PathChange::Noop { .. }))
                .count(),
            2,
            "A 与 B 都原地不动：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_actually_writes_the_whole_value_into_hkcu() {
        let (block, _process, registry) = user_path(r"A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "C"])).expect("用户级");
        assert_eq!(plan.after_raw, r"A;C");
        let applied = apply_rewrite(&registry, &plan, || 0).expect("写用户级不需要提权");
        assert!(applied.wrote);
        assert_eq!(applied.reg_type, RegType::Sz);
        assert_eq!(applied.chars, plan.after_raw.chars().count());
        // **从注册表重读**，而不是相信返回值 —— 声称写了 ≠ 真的写了。
        assert_eq!(reread(&registry), RegValue::Sz(r"A;C".to_owned()));
    }

    #[test]
    fn rewrite_that_matches_the_current_value_writes_nothing() {
        let (block, _process, registry) = user_path(r"A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "B"])).expect("用户级");
        assert!(!plan.changes_value);
        assert!(!plan.will_write());
        let applied = apply_rewrite(&registry, &plan, || 0).expect("用户级");
        assert!(!applied.wrote);
        // 一个字节都没被写：假后端里的值仍然是原来那条。
        assert_eq!(reread(&registry), RegValue::Sz(r"A;B".to_owned()));
    }

    #[test]
    fn rewrite_uses_expand_sz_when_the_value_contains_a_variable() {
        let (block, _process, registry) = user_path(r"A", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&[r"%USERPROFILE%\bin"]))
            .expect("用户级");
        assert_eq!(plan.after_type, RegType::ExpandSz);
        assert_eq!(plan.after_raw, r"%USERPROFILE%\bin");
        let applied = apply_rewrite(&registry, &plan, || 0).expect("用户级");
        assert!(applied.wrote);
        assert_eq!(applied.reg_type, RegType::ExpandSz);
        // **原样**写 `%VAR%`，不许展开（`setx` 的罪状之一就是永久展开）。
        assert_eq!(
            reread(&registry),
            RegValue::ExpandSz(r"%USERPROFILE%\bin".to_owned())
        );
    }

    #[test]
    fn rewrite_refuses_the_machine_scope_on_both_sides() {
        let (block, _process, _registry) = user_path(r"A", RegType::Sz);
        let plan_error = plan_rewrite(&block, EnvScope::Machine, &entries(&["A", "B"]))
            .expect_err("机器级要提权，tuoen 不做静默提权");
        assert!(
            matches!(&plan_error, PlatformError::Unsupported { what } if what.contains("machine")),
            "报的是 Unsupported 且带上作用域：{plan_error}"
        );
        // 计划侧拒绝不算数 —— **写侧**也必须拒绝，否则手写一份 `scope: Machine`
        // 的计划就能绕过它（这正是 `apply_rewrite` 存在的理由）。
        let mut forged =
            plan_rewrite(&block, EnvScope::User, &entries(&["A", "B"])).expect("用户级");
        forged.scope = EnvScope::Machine;
        let apply_error =
            apply_rewrite(&FakeRegistry::default(), &forged, || 0).expect_err("机器级必须被拒");
        assert!(
            matches!(&apply_error, PlatformError::Unsupported { what } if what.contains("machine")),
            "报的是 Unsupported 且带上作用域：{apply_error}"
        );
    }

    #[test]
    fn rewrite_may_empty_the_whole_value_because_that_is_a_choice() {
        // 清空是用户的选择，不是错误 —— 与"条目含 `;`"是两件不同的事。
        let (block, _process, registry) = user_path(r"A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &[]).expect("清空是合法的");
        assert_eq!(plan.after_raw, "");
        assert!(plan.changes_value);
        let applied = apply_rewrite(&registry, &plan, || 0).expect("用户级");
        assert!(applied.wrote);
        assert_eq!(applied.chars, 0);
        assert_eq!(reread(&registry), RegValue::Sz(String::new()));
    }

    #[test]
    fn rewrite_refuses_an_entry_that_contains_the_separator() {
        let (block, _process, _registry) = user_path(r"A", RegType::Sz);
        let error = plan_rewrite(&block, EnvScope::User, &entries(&[r"C:\a;b"]))
            .expect_err("`;` 是分隔符，引号保护不了它");
        assert!(
            matches!(&error, PlatformError::Unsupported { what } if what.contains(r"C:\a;b")),
            "报的是 Unsupported 且带上那条输入：{error}"
        );
        // 与 `plan_add` 同一立场（判据只有一处措辞，两处行为必须一样）。
        assert!(plan_add(&block, r"C:\a;b").is_err());
    }

    #[test]
    fn rewrite_passes_the_broadcast_receipt_through_unchanged() {
        let (block, _process, registry) = user_path(r"A", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "B"])).expect("用户级");
        let called = Cell::new(0_u32);
        let applied = apply_rewrite(&registry, &plan, || {
            called.set(called.get() + 1);
            7
        })
        .expect("用户级");
        assert!(applied.wrote);
        assert_eq!(called.get(), 1, "广播只发一次");
        assert_eq!(applied.broadcast_replies, 7, "回执原样透传，不加工");
    }

    #[test]
    fn rewrite_never_reports_a_replace_for_a_pure_reorder() {
        // 第一阶段（同下标配对）只认"值在本下标上"的条目，于是**换位**必须由
        // 后续阶段认走：反序时第 0 格的新值 `C:\Dev\jdk-21` 属于**挪过来的**
        // 那一条，而它与同下标上的旧值 `C:\Dev\jdk-17` 一个字都不像 —— 若无
        // 这个阶段，它们会被误报成两条 `Replace`（"这一格被改写了"）。
        let (block, _process, _registry) = user_path(r"C:\Dev\jdk-17;C:\Dev\jdk-21", RegType::Sz);
        let plan = plan_rewrite(
            &block,
            EnvScope::User,
            &entries(&[r"C:\Dev\jdk-21", r"C:\Dev\jdk-17"]),
        )
        .expect("用户级");
        assert_eq!(plan.after_raw, r"C:\Dev\jdk-21;C:\Dev\jdk-17");
        assert!(
            plan.changes
                .iter()
                .all(|change| matches!(change, PathChange::Move { .. } | PathChange::Noop { .. })),
            "反序是两条 Move，一条 Replace 都不该有：{:?}",
            plan.changes
        );
        assert_eq!(
            plan.changes
                .iter()
                .filter(|change| matches!(change, PathChange::Move { .. }))
                .count(),
            2,
            "两条都换了位置：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_calls_an_unrelated_same_index_change_a_replace() {
        // **"这两个目录算不算同一个"是一个没有判据的判断，所以这里根本不判。**
        // 同下标 + 字符串不同就是 `Replace`：`C:\Dev\jdk-17` → `C:\Dev\doxygen`
        // 一个字都不像，照样报改写。
        //
        // 这条用例的前身是一条"≥50% 段相同才算改写"的启发式，它会把这个例子
        // 判成"恰好过线"（`C:` + `Dev` = 1/2）—— 于是既没能排除不像的，又要为
        // "多像才算像"再编一个阈值。现在没有阈值可编。
        let (block, _process, _registry) = user_path(r"C:\Dev\jdk-17;C:\x", RegType::Sz);
        let plan = plan_rewrite(
            &block,
            EnvScope::User,
            &entries(&[r"C:\Dev\doxygen", r"C:\x"]),
        )
        .expect("用户级");
        assert_eq!(
            plan.changes.first(),
            Some(&PathChange::Replace {
                value: r"C:\Dev\doxygen".to_owned(),
                was: r"C:\Dev\jdk-17".to_owned(),
                at: 0,
            }),
            "同下标值不同就是改写，不看像不像：{:?}",
            plan.changes
        );
        // `Add` / `Remove` 只留给"只在一侧出现"的条目。
        assert!(
            plan.changes.iter().all(|change| !matches!(
                change,
                PathChange::Add { .. } | PathChange::Remove { .. }
            )),
            "两条值都在两侧出现，一件增删都不该有：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_reports_a_replace_for_a_case_only_change() {
        // 大小写改写：Windows 上同一个目录，报 `Replace` 而不是一删一加。
        let (block, _process, _registry) = user_path(r"C:\tools;C:\x", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&[r"C:\Tools", r"C:\x"]))
            .expect("用户级");
        assert_eq!(plan.after_raw, r"C:\Tools;C:\x");
        assert!(
            plan.changes.contains(&PathChange::Replace {
                value: r"C:\Tools".to_owned(),
                was: r"C:\tools".to_owned(),
                at: 0,
            }),
            "只换了大小写也是改写：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_only_accepts_the_user_scope() {
        // `ProcessOnly` 从来不是"可以写"的作用域：它**不在任何注册表里**，
        // 拿它来计划写回在任何情形下都是错的。
        let (block, _process, _registry) = user_path(r"A", RegType::Sz);
        let plan_error = plan_rewrite(&block, EnvScope::ProcessOnly, &entries(&["A"]))
            .expect_err("进程注入项不在注册表里，写不了");
        assert!(
            matches!(&plan_error, PlatformError::Unsupported { .. }),
            "报的是 Unsupported：{plan_error}"
        );
        let mut forged = plan_rewrite(&block, EnvScope::User, &entries(&["A"])).expect("用户级");
        forged.scope = EnvScope::ProcessOnly;
        assert!(
            apply_rewrite(&FakeRegistry::default(), &forged, || 0).is_err(),
            "写侧也拒绝"
        );
    }

    #[test]
    fn rewrite_does_not_expand_the_before_value_either() {
        // 改之前的值从注册表原样读进来（`REG_SZ` 里字面的 `%USERPROFILE%`
        // **不许**被展开成 `C:\Users\<名字>`）—— 展开是不可逆的信息损失，
        // 而它会让 `changes_value` 与 `Replace.was` 同时变成假的。
        let (block, _process, _registry) = user_path(r"%USERPROFILE%\bin;A", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&[r"%USERPROFILE%\bin"]))
            .expect("用户级");
        assert_eq!(
            plan.before_raw, r"%USERPROFILE%\bin;A",
            "改之前的原文必须逐字节原样"
        );
        assert!(
            plan.changes.contains(&PathChange::Remove {
                value: "A".to_owned(),
                was_at: 1,
            }),
            "只删掉 A：{:?}",
            plan.changes
        );
    }

    #[test]
    fn rewrite_budget_counts_the_machine_scope_too() {
        // 与 `finish_plan`（`plan_add` / `plan_remove` 走它）**同一个口径**：
        // 机器级原文 + 重建后的用户级原文。不含进程注入项，所以仍是下界。
        // 机器级取不到时当 0（那是另一个用例的形状）。
        let machine = r"C:\Windows;C:\Windows\System32";
        let (block, _process, _registry) = user_path_with_machine(r"A;B", machine, RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "C"])).expect("用户级");
        let expected = machine.chars().count() + plan.after_raw.chars().count();
        assert_eq!(
            plan.budget_after.effective_chars,
            expected,
            "机器级 {} 字符必须算进去（重建后用户级 {} 字符）",
            machine.chars().count(),
            plan.after_raw.chars().count()
        );
        assert_eq!(plan.budget_after.registry_chars, expected);
        assert_eq!(plan.budget_after.cliff, PATH_CLIFF_CMD);
        assert_eq!(plan.budget_after.level, BudgetLevel::Ok);
    }

    #[test]
    fn rewrite_budget_is_a_lower_bound_when_there_is_no_machine_path() {
        // 机器级没有 `Path` 时当 0：两个数都只剩重建后的用户级。
        let after = r"A;C";
        let (block, _process, _registry) = user_path(r"A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "C"])).expect("用户级");
        assert_eq!(plan.budget_after.effective_chars, after.chars().count());
        assert_eq!(plan.budget_after.registry_chars, after.chars().count());
        assert_eq!(plan.budget_after.level, BudgetLevel::Ok);
    }

    #[test]
    fn rewrite_that_matches_the_current_value_does_not_even_broadcast() {
        // 计划与现状一致时**不写、也不广播** —— 广播是全局副作用（打到每一个
        // 顶层窗口）。判据是"计数闭包一次都没被调用"，而不只是"回执是 0"。
        //
        // **这条用例的强度限制，要如实说**：`FakeRegistry` 是"直接改内存表、
        // 不做任何加工"的（`fixture.rs` 的 `set_value`），它**没有写入计数**，
        // 所以"一个字节都没被写"只能靠"写完之后读回来还是旧值"间接证明
        // （见 `rewrite_that_matches_the_current_value_writes_nothing`）。
        // 真正的"没写"由真机探针证明，不在这里。
        let (block, _process, registry) = user_path(r"A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["A", "B"])).expect("用户级");
        assert!(!plan.will_write());
        let called = Cell::new(0_u32);
        let applied = apply_rewrite(&registry, &plan, || {
            called.set(called.get() + 1);
            7
        })
        .expect("用户级");
        assert!(!applied.wrote);
        assert_eq!(applied.broadcast_replies, 0, "回执是 0");
        assert_eq!(called.get(), 0, "广播闭包一次都不该被调用");
    }

    #[test]
    fn rewrite_of_an_empty_before_into_an_empty_after_is_a_pure_noop() {
        // 两侧都空的角落：`changes` 为空（连 `DropEmptySegments` 都没有 ——
        // 没有空段可清）、`changes_value == false`、`will_write()` 为 `false`。
        // 这时"清空一条本来就空的 `PATH`"不是错误，也不该写、不该广播。
        let (block, _process, registry) = user_path("", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &[]).expect("合法");
        assert_eq!(plan.before_raw, "");
        assert_eq!(plan.after_raw, "");
        assert!(plan.changes.is_empty(), "没有任何变更：{:?}", plan.changes);
        assert!(!plan.changes_value);
        assert!(!plan.will_write());
        let called = Cell::new(0_u32);
        let applied = apply_rewrite(&registry, &plan, || {
            called.set(called.get() + 1);
            7
        })
        .expect("用户级");
        assert!(!applied.wrote);
        assert_eq!(called.get(), 0);
    }

    /// 把条目原文折成"值的**重数**"表（大小写折叠后按值计数）。
    fn value_counts(raw: &str) -> BTreeMap<String, usize> {
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for entry in parse_entries(raw) {
            if entry.is_empty() {
                continue;
            }
            *counts.entry(entry.key()).or_default() += 1;
        }
        counts
    }

    /// 这一份计划满足"**消失必须被报出来**"这条不变量吗。返回违规的描述。
    ///
    /// **按重数判，不按集合判** —— 集合判法在这三种形状上是空的（两侧的值集合
    /// 完全相同），一个空断言等于没断言。判据是：某个值在 before 里出现的次数
    /// 比在 after 里多，那多出来的每一次都必须有一个说法：
    ///
    /// - `Remove { value }`：这个值被删掉了一次；
    /// - `Replace { was }`：这一格从它变成了别的 —— 那个旧值同样有了说法
    ///   （`Replace` 不表示"消失"，但它确实交代了这一次出现的去向）。
    ///
    /// 两种说法的**总数**必须覆盖重数差。覆盖不了 = 计划漏了一次删除，
    /// 而那是这份变更清单最坏的一种错（用户读到的是一份漏了删除的计划）。
    fn uncovered_disappearances(plan: &PathPlan) -> Vec<String> {
        let before = value_counts(&plan.before_raw);
        let after = value_counts(&plan.after_raw);
        let mut violations = Vec::new();
        for (key, before_times) in &before {
            let after_times = after.get(key).copied().unwrap_or(0);
            if *before_times <= after_times {
                continue;
            }
            let accounted = plan
                .changes
                .iter()
                .filter(|change| match change {
                    PathChange::Remove { value, .. } => {
                        normalize_entry(value.trim_matches('"')).to_lowercase() == *key
                    }
                    PathChange::Replace { was, .. } => {
                        normalize_entry(was.trim_matches('"')).to_lowercase() == *key
                    }
                    _ => false,
                })
                .count();
            let missing = before_times - after_times;
            if accounted < missing {
                violations.push(format!(
                    "`{key}`：before {before_times} 次、after {after_times} 次，缺 {missing} 次，\
                     但 `Remove` + `Replace.was` 只覆盖了 {accounted} 次"
                ));
            }
        }
        violations
    }

    /// 把 `before` → `after` 这一对跑一遍计划，并把不变量断言掉。
    fn assert_removal_invariant(before: &str, after_entries: &[&str]) {
        let (block, _process, _registry) = user_path(before, RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(after_entries)).expect("用户级");
        let violations = uncovered_disappearances(&plan);
        assert!(
            violations.is_empty(),
            "规划 `{before}` → `{}` 时，消失的条目没有被报出来：{violations:?}\n\
             变更清单：{:?}",
            plan.after_raw,
            plan.changes
        );
    }

    #[test]
    fn a_value_that_disappears_is_always_reported_as_removed() {
        // **不变量**（与算法无关，是这份清单必须成立的性质）：
        // `before` 里有、`after` 里没有的**每一次出现**都必须有一个说法 ——
        // `Remove`，或者一次把它写成别人的 `Replace`（`Replace.was` 承载了它）。
        // 覆盖不了就是漏了一次删除，而用户读到的会是一份不完整的计划。
        //
        // 判据按**重数**而不是按集合：这三种形状两侧的**值集合完全相同**
        // （都只是 A 与 B），集合判法在这里恒为真 —— 一个空断言等于没断言。
        //
        // 三种形状各有各的难处：
        // ① `A;B;A`：重复的那次落在**别人的格子**上（新列表第 0 格被第一次的 A 占了）；
        // ② `A;;A;B`：重复的那次落在**被 `Move` 定过结论的格子**上；
        // ③ `A;A;B`：重复的那次落在**被 `Replace` 定过结论的格子**上 —— 这一种
        //    最微妙：第三遍会撤掉第二遍的 `Remove`，于是那次 A 的去向由
        //    `Replace.was` 承载；而真正只在 before 里存在的 `B`（第一次出现的
        //    那一条被换成了 A）另有 `Remove`。
        assert_removal_invariant(r"A;B;A", &["A", "B"]);
        assert_removal_invariant(r"A;;A;B", &["A", "B"]);
        assert_removal_invariant(r"A;A;B", &["B", "A"]);
    }

    #[test]
    fn a_duplicate_that_lands_on_a_rewritten_slot_is_still_removed() {
        // 形状 ③ 的细节版：`A;A;B` → `B;A`。B 从原文第 2 段挪到新列表第 0 段；
        // 新列表第 1 格原地不动（A）；**多出来的那次 A 必须被删掉并指向它自己的
        // 原文下标**（第 0 段）—— 它不可能指向别处，因为它就是要消失的那一次。
        let (block, _process, _registry) = user_path(r"A;A;B", RegType::Sz);
        let plan = plan_rewrite(&block, EnvScope::User, &entries(&["B", "A"])).expect("用户级");
        assert_eq!(plan.after_raw, r"B;A");
        assert!(
            plan.changes.contains(&PathChange::Move {
                value: "B".to_owned(),
                was_at: 2,
                at: 0,
            }),
            "B 从第 2 段挪到第 0 段：{:?}",
            plan.changes
        );
        assert!(
            plan.changes.contains(&PathChange::Remove {
                value: "A".to_owned(),
                was_at: 0,
            }),
            "多出来的那次 A 归 Remove，was_at 是它自己的原文下标 0：{:?}",
            plan.changes
        );
        assert!(
            plan.changes.contains(&PathChange::Noop {
                value: "A".to_owned(),
                reason: NoopReason::AlreadyPresent,
            }),
            "第 1 段那次 A 原地不动：{:?}",
            plan.changes
        );
    }

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
