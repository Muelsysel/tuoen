//! `tuoen.d/` 的**磁盘格式**。
//!
//! 这个文件是 `capture` 与未来 `restore` / `doctor` 之间的**契约本身**：结构体就是
//! 文件里长什么样，字段名就是 TOML 键名。改这里等于改一个对外的文件格式 ——
//! 所以每一处偏离"看起来更整齐"的写法，都在这里写清了理由。
//!
//! # 三条贯穿全文件的规则
//!
//! 1. **每个文件都带 `schema_version` 与 `captured_at`，且两者都在最前面。**
//!    `toml` 的序列化器要求标量出现在任何表 / 表数组**之前**，所以字段顺序不是风格问题
//!    （把 `[[entry]]` 放在 `schema_version` 前面，序列化会直接报错）。
//! 2. **`Option::None` 一律 `skip_serializing_if`。** TOML 没有 `null`，一个没有
//!    这一个属性的 `Option` 字段会让整个文件写不出来。
//! 3. **所有"我们说不准"的地方都是字符串枚举而不是布尔。** 见 [`Existence`]。
//!
//! # 为什么 `skipped.toml` 是一个独立文件而不是每个 section 里的一段
//!
//! "跳过了什么、为什么"是**跨 section** 的一句话（本票扫环境变量，后续票扫配置文件），
//! 而用户问的是"这份快照是不是完整的"—— 那个问题只有一个答案。放在一个文件里，
//! 加一个新 section 的跳过项**不需要动任何已有文件的结构**（票据 #12 的硬要求）。

use std::fmt;

use serde::{Deserialize, Serialize};
use tuoen_platform::{EnvScope, RegType, ReparseKind};

/// 磁盘格式的版本。**改了文件形状就要加一**，`restore` 靠它拒绝读不认识的格式。
pub const SCHEMA_VERSION: u32 = 1;

/// 一个路径"在不在"。
///
/// # 为什么不是 `bool`
///
/// 票据给的骨架里是 `exists = true`。实现时它必须变成三态，理由是**布尔会合并两个
/// 相反的结论**：本机 `PATH` 上有 `%NOPE%\bin` 这种条目，我们**故意不展开**用户
/// 环境变量（展开是不可逆的信息损失，见 `AGENTS.md` 规矩二），所以对它只能答
/// "不知道"。写成 `exists = false` 是在说"我们看过了，它不在"——
/// 而我们没看过，也不打算猜。**答不了的题不许猜**，所以第三个状态必须存在。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Existence {
    /// 看过了，在。
    Yes,
    /// 看过了，不在。
    No,
    /// 看不了：值里还留着没展开的 `%VAR%`，或者那个作用域的类型声明它不该被展开。
    Unknown,
}

impl Existence {
    /// `--json` 与 TOML 里的稳定取值，**不本地化**。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Existence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// schema.toml
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/schema.toml` —— 这一份快照是什么、谁写的、包含哪些 section。
///
/// **它是 `--only` 的产物清单**：只捕获 `path` 时，这个文件里的 `sections` 就只有
/// `["path"]`，于是"这份快照缺 tools"与"这台机器上没有工具"能被区分开 ——
/// 否则 `restore` 会把"没捕获"读成"没有"。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaFile {
    /// 格式版本。
    pub schema_version: u32,
    /// 捕获时间（RFC 3339，UTC）。**幂等比较时必须忽略它。**
    pub captured_at: String,
    /// 写这份快照的 tuoen 版本。
    pub tuoen_version: String,
    /// 这份快照包含哪些 section（稳定 slug，排序后）。
    pub sections: Vec<String>,
}

impl SchemaFile {
    /// 造一个表头。`sections` 会被排序，所以调用方的顺序不影响文件内容。
    #[must_use]
    pub fn new(captured_at: &str, tuoen_version: &str, mut sections: Vec<String>) -> Self {
        sections.sort();
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: captured_at.to_owned(),
            tuoen_version: tuoen_version.to_owned(),
            sections,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// tools.toml
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/tools.toml` —— 工具与版本，每条带来源与置信度。
///
/// **不允许出现没有来源的条目**（`source` / `confidence` / `evidence` 都是必填）。
/// 三者的取值是 `crates/core/src/detect.rs` 里那几个枚举的稳定 slug ——
/// 存字符串而不是枚举，是为了让未来新增一层置信度**不需要**改这个文件的形状。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolsFile {
    /// 格式版本。
    pub schema_version: u32,
    /// 捕获时间。
    pub captured_at: String,
    /// 工具记录，按（名称，路径，来源，版本）排序 —— 顺序稳定才能比逐字节。
    #[serde(default)]
    pub tool: Vec<ToolRow>,
}

impl ToolsFile {
    /// 造一个只有表头的文件。
    #[must_use]
    pub fn new(captured_at: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: captured_at.to_owned(),
            tool: Vec::new(),
        }
    }
}

/// 一条工具记录。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRow {
    /// 逻辑工具名，小写（`node` / `python` / `java`）。
    pub name: String,
    /// 版本。发现但问不出来时为 `None`（见 `DetectedTool` 的不变量）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 主路径（可执行文件或安装根目录）。
    pub path: String,
    /// 怎么被发现的（`tuoen` / `path-resolution` / `app-paths` / …）。
    pub source: String,
    /// 有多可信（`managed` / `executable` / …）。
    pub confidence: String,
    /// 第三方版本管理器（`nvm4w` / …），没有就是 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manager: Option<String>,
    /// 一句话说明这条是怎么被发现的。中文。
    pub evidence: String,
    /// 这条能不能在新机器上由 tuoen 自动重建 —— `confidence` 的投影，存在这里的
    /// 理由是**读快照的人不该被迫重新实现一遍那个判据**。
    pub reproducible: bool,
}

// ─────────────────────────────────────────────────────────────────────────────
// path.toml
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/path.toml` —— `PATH` 的**结构**，不只是一串字符串。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathFile {
    /// 格式版本。
    pub schema_version: u32,
    /// 捕获时间。
    pub captured_at: String,
    /// 长度预算。
    pub budget: PathBudgetRow,
    /// 逐条条目，**按作用域内出现顺序**（顺序本身是数据，不许排序）。
    #[serde(default)]
    pub entry: Vec<PathRow>,
    /// 真实的解析顺序：一串指向 `entry` 的引用。
    ///
    /// **为什么需要一个额外的列表**：作用域内的顺序加上"机器级在前、用户级在后"
    /// 这条规则**推不出**真实的生效顺序 —— 启动器还会在最前面注入条目
    /// （本机实测有 PowerShell 的 MSIX 别名，77 字符，不在任何注册表里）。
    /// 这个列表就是那份真实顺序，`entry` 里查不到的解释都不算数。
    #[serde(default)]
    pub effective: Vec<EntryRefRow>,
}

impl PathFile {
    /// 造一个只有表头的文件。
    #[must_use]
    pub fn new(captured_at: &str, budget: PathBudgetRow) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: captured_at.to_owned(),
            budget,
            entry: Vec::new(),
            effective: Vec::new(),
        }
    }
}

/// `PATH` 的长度与悬崖。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathBudgetRow {
    /// 用户级注册表原文的字符数（**未展开**）。
    pub raw_user_chars: usize,
    /// 机器级注册表原文的字符数（**未展开**）。
    pub raw_machine_chars: usize,
    /// 真实生效的那条 `PATH` 的字符数（进程口径，含注入项）。
    pub effective_chars: usize,
    /// `cmd.exe` 的悬崖。
    pub cliff: usize,
    /// 距悬崖还剩多少字符（`cliff` 扣掉 `effective_chars`；超限时为 0）。
    pub remaining: usize,
    /// 档位：`ok` / `warning` / `critical` / `exceeded`。
    pub level: String,
}

/// 一个 `PATH` 条目。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRow {
    /// `user` / `machine` / `process-only`。
    pub scope: EnvScope,
    /// 在这个作用域里的位置（0 起）。**空条目也占一个位置**（本机 HKLM 的 `Path` 里有 `;;`）。
    pub index: usize,
    /// 这个目录归谁：`tuoen` / `third-party` / `system` / `unknown`。
    ///
    /// 判据写在 `collect/path.rs` 的模块文档里，**只有三条**：
    /// 我们自己的根 → `tuoen`；`%SystemRoot%` 里 → `system`；本身是个 reparse point
    /// （别人的版本切换器）→ `third-party`；其余一律 `unknown`。
    /// **`unknown` 是默认值，不是失败。**
    ///
    /// 曾经考虑过第四条"能归属到某个已检测到的工具"—— 砍掉它有两个理由：
    /// 它会让 `path.toml` 的内容依赖 `tools.toml` 的检测结果（两棵树互相牵制，
    /// 而两条记录各自都要求确定性），而且它要**再跑一次检测**（再 spawn 一遍进程），
    /// 与"探测只经过注入的运行器"那条用例直接冲突。
    pub owner: String,
    /// 注册表 / 环境块里的**原文**。含引号的条目保留引号。
    pub raw: String,
    /// Windows 会把它解析成什么。**取决于类型**：`REG_SZ` 的值不展开（`raw` 就是答案），
    /// `REG_EXPAND_SZ` 的值才展开。写成"总是展开"会让一个 `REG_SZ` 里字面含 `%` 的条目
    /// 显示成一个根本不存在的路径。
    pub expanded: String,
    /// 原文是否被引号包着 —— 还原时要保持原样。
    pub quoted: bool,
    /// 这是一段**空条目**（`PATH` 里的 `;;`）。
    ///
    /// 空条目**不是一个目录**：Windows 的历史行为是把它当"当前目录"，
    /// 所以"它存不存在"这个问题本身不成立 —— 空条目的 `exists` 恒为 `unknown`
    /// 且**不问磁盘**（真机验收抓出来的：第一版让它走正常分支，磁盘对空串答
    /// "不存在"，于是它被记成一条失效条目）。`doctor` 要判"这里缺一个目录"，
    /// 判据必须是 `!empty && exists == "no"`。
    pub empty: bool,
    /// 注册表类型（`sz` / `expand-sz`）。进程注入条目没有类型，为 `None`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reg_type: Option<RegType>,
    /// 在不在。**空条目恒为 `unknown`**（见 [`Self::empty`]）。
    pub exists: Existence,
    /// reparse 形状（`none` / `junction` / `symlink-dir` / `symlink-file` /
    /// `app-exec-alias` / `other:0x…`）。**`app-exec-alias` 必须能出现**：
    /// 它是 0 字节、`Test-Path` 会通过的那种东西。
    pub reparse: ReparseKind,
    /// 链接目标（junction / symlink）。**只记录，不跟随**。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link_target: Option<String>,
    /// 值里含 `%`。
    pub has_vars: bool,
    /// 值里硬编码了 `C:\Users\<某个名字>\`（含 `%` 就不算 —— 那是可搬运的写法）。
    pub has_username: bool,
    /// 同一个值在这个快照里第几次出现（0 = 首次）。
    ///
    /// **"同值条目里的序号"**（票据原话），所以一个从没重复过的值恒为 `0`，
    /// 第二次出现是 `1`，第三次 `2` —— 于是 `dup_index > 0` 的行数**就是**
    /// 重复条目的条数。比较时去尾部反斜杠并忽略大小写（`C:\shared\` 与 `c:\SHARED`
    /// 是同一个值），计数跨三个作用域。
    pub dup_index: usize,
}

/// 一个指向 [`PathRow`] 的引用。
///
/// 两个坐标系统的存在理由见 `tuoen_platform::path::EntryRef`：注册表作用域里
/// `index` 是**作用域内**的位置，`process-only` 里是**进程 `PATH` 内**的位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryRefRow {
    /// 作用域。
    pub scope: EnvScope,
    /// 作用域内的位置。
    pub index: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// env.toml
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/env.toml` —— 持久环境变量（用户级 + 机器级）。
///
/// **为什么只有这两个作用域**：进程环境不是可搬运的状态，它是"这一台机器此刻的
/// 结果"。把 68 个进程变量写进快照只会让 diff 全是噪声。
///
/// **但密钥扫描仍然扫进程环境**：看见了一个形似凭据的变量却不说，比把它写进文件更糟。
/// 被跳过的进程级变量会出现在 `skipped.toml` 里，`scope = "process-only"` 说清了
/// 它本来也不会被写。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvFile {
    /// 格式版本。
    pub schema_version: u32,
    /// 捕获时间。
    pub captured_at: String,
    /// 变量，按（名称忽略大小写，作用域）排序。
    #[serde(default)]
    pub var: Vec<EnvVarRow>,
}

impl EnvFile {
    /// 造一个只有表头的文件。
    #[must_use]
    pub fn new(captured_at: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: captured_at.to_owned(),
            var: Vec::new(),
        }
    }
}

/// 一个环境变量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvVarRow {
    /// 变量名。**可以含空格**（本机 `IntelliJ IDEA`）—— 合法，只是很多工具会静默跳过。
    pub name: String,
    /// `user` / `machine`。同名变量出现在两个作用域时**两条都在**。
    pub scope: EnvScope,
    /// 注册表里的原文。**从不展开**。
    pub value_raw: String,
    /// Windows 会解析成什么：`sz` 就是原文，`expand-sz` 才展开。
    pub value_expanded: String,
    /// 注册表类型。
    pub reg_type: RegType,
    /// 值像不像一个绝对路径目标（是的话下面那个字段才有意义）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// 那个目标在不在。`not-a-path` = 值根本不是路径，`unknown` = 展开后还剩 `%VAR%`。
    pub target_exists: TargetExistence,
}

/// 环境变量指向的目标在不在。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TargetExistence {
    /// 看过了，在。
    Yes,
    /// 看过了，不在。
    No,
    /// 值像路径，但展开后还剩 `%VAR%` —— 不猜。
    Unknown,
    /// 值根本不是路径（版本号、枚举、列表）。**这一态是必须的**：本机 `UV_PYTHON=3.13`，
    /// 把它判成"指向的目录不存在"会报出一个纯粹的假问题。
    NotAPath,
}

impl TargetExistence {
    /// 稳定 slug。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Unknown => "unknown",
            Self::NotAPath => "not-a-path",
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// wsl.toml
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/wsl.toml` —— WSL 发行版与它们的**实际** vhdx 路径。
///
/// **必须记实际路径**：本机 `Arch-Linux-current` 的 `BasePath` 是
/// `C:\linux\Arch-Linux-current` —— 手工导入的非标准位置。只 glob 默认目录的工具
/// 会完全漏掉它，而漏掉的恰好是"换电脑时最需要知道它装在哪"的那种。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WslFile {
    /// 格式版本。
    pub schema_version: u32,
    /// 捕获时间。
    pub captured_at: String,
    /// 发行版，按名字排序。
    #[serde(default)]
    pub distribution: Vec<WslRow>,
}

impl WslFile {
    /// 造一个只有表头的文件。
    #[must_use]
    pub fn new(captured_at: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: captured_at.to_owned(),
            distribution: Vec::new(),
        }
    }
}

/// 一个 WSL 发行版。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WslRow {
    /// 发行版名（`Ubuntu-22.04` / `Arch-Linux-current`）。
    pub name: String,
    /// 注册表里的 GUID —— 它是这个发行版的**真实身份**，名字可以被 `wsl --import` 换掉。
    pub guid: String,
    /// `BasePath`（展开后）。
    pub base_path: String,
    /// `BasePath` 不在默认位置（默认位置在 `%LOCALAPPDATA%` 底下）。
    pub non_standard_path: bool,
    /// `Version` 字段：`1` / `2`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wsl_version: Option<u32>,
    /// `State` 字段：`1` = 已安装。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<u32>,
    /// vhdx 的完整路径。
    pub vhdx_path: String,
    /// vhdx 在不在。
    pub vhdx_exists: Existence,
    /// vhdx 的字节数（在的时候才有）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vhdx_bytes: Option<u64>,
}

// ─────────────────────────────────────────────────────────────────────────────
// skipped.toml
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen.d/skipped.toml` —— **我们看见了、但故意没写进去**的东西。
///
/// **静默跳过是 bug**：它会让用户以为"都备份好了"。所以这份清单是快照的一部分，
/// 而不是一条日志。
///
/// # 绝不写材料
///
/// 这里只有**名字**、**位置**与**原因**。一个被跳过的 token 的**值**、
/// 它的前几位、它的长度都不允许出现在这个文件里 —— 判断"它是不是凭据"发生在
/// 写文件之前，而判断的结论是唯一被带出来的东西。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedFile {
    /// 格式版本。
    pub schema_version: u32,
    /// 捕获时间。
    pub captured_at: String,
    /// 跳过项，按（section，scope，name）排序。
    #[serde(default)]
    pub skipped: Vec<SkipEntry>,
}

impl SkippedFile {
    /// 造一个只有表头的文件。
    #[must_use]
    pub fn new(captured_at: &str) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            captured_at: captured_at.to_owned(),
            skipped: Vec::new(),
        }
    }

    /// 加一条并保持排序（排序是幂等性的一部分：两次运行的顺序必须一样）。
    pub fn push(&mut self, entry: SkipEntry) {
        self.skipped.push(entry);
        self.skipped.sort_by(|a, b| {
            (&a.section, &a.scope, a.name.to_lowercase()).cmp(&(
                &b.section,
                &b.scope,
                b.name.to_lowercase(),
            ))
        });
    }
}

/// 一条跳过项。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkipEntry {
    /// 属于哪个 section（`env` / 未来的 `configs`）。
    pub section: String,
    /// 在哪个作用域里看到的（环境变量用；文件用 `Some("file")` 也不违法）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// **名字**（变量名 / 文件的相对位置）。**不是值。**
    pub name: String,
    /// 为什么跳过：`credential` / `unreadable` / …（稳定 slug）。
    pub kind: String,
    /// 一句话原因，中文，**不含任何材料**。
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 每个文件类型都要能"写出去再读回来"。
    ///
    /// **这条用例的存在理由**：TOML 没有 `null`，一个忘了
    /// `skip_serializing_if = "Option::is_none"` 的 `Option` 字段会让**整个文件
    /// 序列化失败**（而不是少一个键）。所以这里把每一种文件都真的序列化一遍 ——
    /// 只要有一个字段写错，这条用例就会红。
    #[test]
    fn every_file_type_round_trips_through_toml() {
        let tools = ToolsFile {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-10-02T12:00:00Z".to_owned(),
            tool: vec![ToolRow {
                name: "node".to_owned(),
                version: Some("24.19.0".to_owned()),
                path: r"C:\nvm4w\nodejs".to_owned(),
                source: "manager".to_owned(),
                confidence: "manager-owned".to_owned(),
                manager: Some("nvm4w".to_owned()),
                evidence: "nvm4w 管着它".to_owned(),
                reproducible: false,
            }],
        };
        let path = PathFile {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-10-02T12:00:00Z".to_owned(),
            budget: PathBudgetRow {
                raw_user_chars: 773,
                raw_machine_chars: 1007,
                effective_chars: 1781,
                cliff: 8191,
                remaining: 6410,
                level: "ok".to_owned(),
            },
            entry: vec![PathRow {
                scope: EnvScope::Machine,
                index: 0,
                owner: "system".to_owned(),
                raw: r"C:\Windows".to_owned(),
                expanded: r"C:\Windows".to_owned(),
                quoted: false,
                empty: false,
                reg_type: Some(RegType::Sz),
                exists: Existence::Yes,
                reparse: ReparseKind::None,
                link_target: None,
                has_vars: false,
                has_username: false,
                dup_index: 0,
            }],
            effective: vec![EntryRefRow {
                scope: EnvScope::Machine,
                index: 0,
            }],
        };
        let env = EnvFile {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-10-02T12:00:00Z".to_owned(),
            var: vec![EnvVarRow {
                name: "IntelliJ IDEA".to_owned(),
                scope: EnvScope::User,
                value_raw: r"C:\Program Files\JetBrains".to_owned(),
                value_expanded: r"C:\Program Files\JetBrains".to_owned(),
                reg_type: RegType::Sz,
                target: Some(r"C:\Program Files\JetBrains".to_owned()),
                target_exists: TargetExistence::No,
            }],
        };
        let wsl = WslFile {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-10-02T12:00:00Z".to_owned(),
            distribution: vec![WslRow {
                name: "Arch-Linux-current".to_owned(),
                guid: "{00000000-0000-0000-0000-000000000001}".to_owned(),
                base_path: r"C:\linux\Arch-Linux-current".to_owned(),
                non_standard_path: true,
                wsl_version: Some(2),
                state: Some(1),
                vhdx_path: r"C:\linux\Arch-Linux-current\ext4.vhdx".to_owned(),
                vhdx_exists: Existence::Yes,
                vhdx_bytes: Some(4_000_000_000),
            }],
        };
        let schema = SchemaFile::new(
            "2026-10-02T12:00:00Z",
            "0.1.0",
            vec!["wsl".to_owned(), "env".to_owned()],
        );
        assert_eq!(schema.sections, ["env", "wsl"], "section 必须排序");

        let mut skipped = SkippedFile::new("2026-10-02T12:00:00Z");
        skipped.push(SkipEntry {
            section: "env".to_owned(),
            scope: Some("user".to_owned()),
            name: "GITLAB_TOKEN".to_owned(),
            kind: "credential".to_owned(),
            reason: "值形似 GitLab 访问令牌".to_owned(),
        });

        macro_rules! round_trip {
            ($value:expr, $ty:ty) => {{
                let text = toml::to_string_pretty(&$value).expect("序列化");
                let back: $ty = toml::from_str(&text).expect("反序列化");
                assert_eq!(back, $value, "往返必须一模一样：\n{text}");
                text
            }};
        }

        let text = round_trip!(schema, SchemaFile);
        assert!(text.starts_with("schema_version = 1"), "{text}");
        round_trip!(tools, ToolsFile);
        let path_text = round_trip!(path, PathFile);
        round_trip!(env, EnvFile);
        round_trip!(wsl, WslFile);
        let skipped_text = round_trip!(skipped, SkippedFile);

        // 表头必须出现在任何表 / 表数组之前，否则 TOML 根本不是合法文档。
        for text in [&path_text, &skipped_text] {
            let head = text.find("schema_version").expect("有表头");
            let body = text
                .find("[[")
                .or_else(|| text.find("[budget]"))
                .expect("有表体");
            assert!(head < body, "标量必须在表之前：\n{text}");
        }
    }

    /// 一个 `None` 的 `Option` 不许变成"少一个键"，更不许让文件写不出来。
    #[test]
    fn absent_optionals_are_omitted_not_written_as_null() {
        let file = ToolsFile {
            schema_version: SCHEMA_VERSION,
            captured_at: "2026-10-02T12:00:00Z".to_owned(),
            tool: vec![ToolRow {
                name: "python".to_owned(),
                version: None,
                path: r"C:\Python312".to_owned(),
                source: "filesystem-scan".to_owned(),
                confidence: "directory-only".to_owned(),
                manager: None,
                evidence: "扫到的目录".to_owned(),
                reproducible: false,
            }],
        };
        let text = toml::to_string_pretty(&file).expect("序列化");
        // 注意别把 `schema_version` 里的 `version` 当成命中 —— 按行首匹配。
        assert!(!text.contains("\nversion ="), "{text}");
        assert!(!text.contains("\nmanager ="), "{text}");
        assert!(!text.contains("null"), "{text}");
    }
}
