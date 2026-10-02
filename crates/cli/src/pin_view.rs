//! 项目级 pin 那一族的 **`--json` 形状**与**人类输出**。
//!
//! 与 `doctor_view` / `capture_view` 的做法一致：命令族一个 `*_view.rs`，
//! 视图与打印都在里面，编排在 [`crate::shell_cmd`] / [`crate::trust_cmd`] /
//! [`crate::lock_cmd`]。
//!
//! # 三条硬规矩
//!
//! 1. **`--json` 的成功载荷里不出现中文**（决策 35）。所以每个取值都是**稳定 slug**：
//!    `shell` 是 `cmd` / `powershell`（[`tuoen_core::pin::ShellKind::as_str`]）、
//!    `state` 是 `trusted` / `stale` / `missing-file`、`action` 是
//!    `trusted` / `revoked` / `absent`。中文只在**人类输出**里。
//! 2. **`message` 不进 `--json`** —— 它天生是一句中文（含"下一步该跑什么命令"），
//!    而机器要的判据是 `error.code` 那个 slug。两条出口各取所需：
//!    **人**读人类输出，**脚本**读 [`ShellPlanView`] 这一族。
//! 3. **视图不直接序列化 core 的结构体**。`ResolvedTool` 的字段名是 Rust 的，
//!    而 `--json` 的键名是**对外契约** —— 让前者变成事实上的公开 API，
//!    正是 `main.rs` 的模块文档点名要避免的事。
//!
//! # `Option` 字段的取舍（**冻结，契约测试逐条钉住**）
//!
//! 本族有**两类**可选字段，它们的取舍**刻意不同**：
//!
//! ## 一、`null`：答案是"没有"（不省略键）
//!
//! `exec` / `manager` / `warnings[].winner` / `warnings[].entry` 一律序列化成
//! `null`，**不省略**。理由：这四个位置的 `null` 是一个**真实的答案** ——
//! "这一次没有给 `--exec`"、"这个工具不由任何第三方版本管理器管"、
//! "没有任何目录赢到这个命令"（决策 120 明确要求 `winner = None` 这一态
//! **单独可辨**，不许合并成空串，也不许与"问过了但没答出来"混起来）。
//!
//! 一个会**时有时无**的键会让消费者写出 `if key in obj` 那种分支，
//! 而那个分支恰好是"这一条没问"与"这一条问了、答案是空"最容易混掉的地方。
//! 冻结形状里也已经写着 `"exec":null` 与 `"manager":null`。
//!
//! ## 二、省略键：这个命令**根本不问**这件事
//!
//! `trust` **只在 `auto` 的载荷里出现**（`shell` 的载荷里连键都没有）。
//! 理由：`shell` 永远可用、不需要信任（决策 15），所以它**没有**"查过信任状态"
//! 这件事 —— 一个恒为 `null` 的 `trust` 键会让消费者以为 `shell` 也查了那道门，
//! 只是没查到。缺席 = 不问，null = 答案是"没有"：这两件事必须分得开
//! （与 `doctor_view` 里 `confidence` 的 `skip_serializing_if` 同一条规矩）。
//!
//! ## 三、`hash` 根本不进 `--json`
//!
//! `ResolvedTool::hash` 是**锁文件**的字段（决策 115：`[[tool]]` 里带 `hash`），
//! 冻结的 `--json` 工具形状里没有它（`{name, spec, version, source, manager, path}`）。
//! 机器要哈希请读 `tuoen.lock` —— 那里它是**逐字节可 diff** 的 TOML，
//! 而不是被 JSON 转义过的一串。
//!
//! # 工具表里**没有** `installed`
//!
//! "这个工具还装了哪些版本"只在**失败**时才有意义（`version-not-installed` 的
//! 中文消息里会列出来），成功载荷里带一份完整版本清单会让每次 `shell --dry-run`
//! 的输出随机器上装了多少东西而变 —— 而它的消费者只想知道"这一次生效的是哪个"。
//!
//! # 失败也是输出，所以 [`Failure`] 也在这里
//!
//! 本族四个命令（`shell` / `auto` / `trust` / `lock`）的失败形状是**同一个**：
//! 一个稳定的机器可读码 + 一句中文（含下一步该跑什么命令）。
//! 三份各写一份的结果是某一天 `tuoen trust` 与 `tuoen auto` 对同一件事
//! 说两句不同的话，而用户会以为它们是两件事。
//!
//! 它放在这个模块而不是某个 `*_cmd` 里，是因为 `*_cmd` 之间会互相引用
//! （`lock_cmd` 要用 `shell_cmd` 的解析器，`shell_cmd` 要用 `lock_cmd` 的写锁器），
//! 而**共用的输出形状必须有一个不属于任何一方的落点**。

use std::path::{Path, PathBuf};

use serde::Serialize;
use tuoen_core::pin::{
    LockFile, PinError, ResolvedTool, ShadowWarning, ShellPlan, ShellSpec, TrustEntry, TrustState,
};

use crate::envelope::Envelope;
use crate::exit;

// ─────────────────────────────────────────────────────────────────────────────
// 共用的小视图
// ─────────────────────────────────────────────────────────────────────────────

/// 一个已解析工具的形状。**字段就是冻结形状的那六个键**（多一个少一个都是破坏契约）。
///
/// `manager` 恒在、无第三方管理器时是 `null`（见模块文档的第一类）。
///
/// **别指望输出里键的次序**：`Envelope::ok` 走的是 `serde_json::to_value`，
/// 而 `serde_json` 默认把对象存进 `BTreeMap` —— 发出去的 JSON 里键是**按字典序**
/// 排的（`manager` / `name` / `path` / `source` / `spec` / `version`）。
/// 这是本仓库所有命令的既有行为，JSON 的对象本来也不承诺次序；
/// 这里写清它是为了让下一个人不用再猜一遍。契约测试按**键**断言，不按次序。
#[derive(Debug, Serialize)]
pub struct ToolView {
    /// 逻辑工具名（小写）。
    pub name: String,
    /// **声明里的原文**（如 `"24"`），不是解析出来的版本 —— 用户要看的是自己写了什么。
    pub spec: String,
    /// 解析出来的精确版本。
    pub version: String,
    /// 这个版本是哪来的（kebab slug：`tuoen` / `path-resolution` / `manager` …）。
    pub source: String,
    /// 第三方版本管理器名（`nvm4w` 之类），无则 `null`。
    pub manager: Option<String>,
    /// 要被前置进子进程 `PATH` 的目录（或可执行文件所在目录）。
    pub path: String,
}

impl From<&ResolvedTool> for ToolView {
    fn from(tool: &ResolvedTool) -> Self {
        Self {
            name: tool.name.clone(),
            spec: tool.spec.clone(),
            version: tool.version.clone(),
            source: tool.source.clone(),
            manager: tool.manager.clone(),
            path: tool.path.display().to_string(),
        }
    }
}

fn tools_view(tools: &[ResolvedTool]) -> Vec<ToolView> {
    tools.iter().map(ToolView::from).collect()
}

/// 一条遮蔽警告的形状。
///
/// `winner` 与 `entry` **恒在**，可能为 `null`：`winner = None` 表示
/// "整条 `pathAfter` 里没有任何目录有这个命令"（布局猜错了）——
/// 决策 120 明确要求这一态**单独可辨**。
#[derive(Debug, Serialize)]
pub struct WarningView {
    /// 哪个命令没生效。
    pub command: String,
    /// 赢了的那条目录值。`null` = 前置之后**没有任何目录**有这个命令。
    pub winner: Option<String>,
    /// 赢家那条条目的原文（如 `node.exe`）。
    pub entry: Option<String>,
}

impl From<&ShadowWarning> for WarningView {
    fn from(warning: &ShadowWarning) -> Self {
        Self {
            command: warning.command.clone(),
            winner: warning.winner.clone(),
            entry: warning.entry.clone(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `shell` / `auto`
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen shell --dry-run --json` 与 `tuoen auto --dry-run --json` 的成功载荷。
///
/// 两个命令共用这一个类型（决策 121：它们共用同一个计划构造器），
/// 唯一的差别是 [`Self::trust`] 只在 `auto` 里出现。
#[derive(Debug, Serialize)]
pub struct ShellPlanView {
    /// 计划是为哪个目录算的（绝对路径）。
    pub cwd: String,
    /// `cmd` / `powershell`（**不本地化**）。
    pub shell: &'static str,
    /// 要跑的那条命令；交互式模式（不带 `--exec`）时是 `null`。
    pub exec: Option<String>,
    /// 当前深度（读自 `TUOEN_SHELL_DEPTH`，坏值当 0）。
    pub depth: u32,
    /// 前置之前的 `PATH`。
    #[serde(rename = "pathBefore")]
    pub path_before: String,
    /// 子进程会拿到的 `PATH`（前置目录 + 去掉重复后的原 `PATH`）。
    #[serde(rename = "pathAfter")]
    pub path_after: String,
    /// 被前置的目录，**按工具 id 字典序**（决策 119：TOML 表的顺序不是契约）。
    pub prepend: Vec<String>,
    /// 这一次生效的每个工具。
    pub tools: Vec<ToolView>,
    /// "前置了却没生效"的那几条（决策 120）。空数组 = 都生效了。
    pub warnings: Vec<WarningView>,
    /// 这一次调用**真的写了** `tuoen.lock` 吗。
    ///
    /// `--dry-run` 下恒为 `false` —— 不是"没写成功"，而是"这一次的任务里没有写"。
    #[serde(rename = "lockWritten")]
    pub lock_written: bool,
    /// 这道信任门的结果。**只有 `auto` 有这个键**（见模块文档的第二类）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trust: Option<&'static str>,
}

impl ShellPlanView {
    /// 从计划与 shell 规格组装。
    ///
    /// **每一个值都从 `plan` / `spec` 里取**，这一层不重新算一遍 ——
    /// 算两遍就会出现"报告里是这一次、执行的是那一次"。
    #[must_use]
    pub fn new(
        plan: &ShellPlan,
        spec: &ShellSpec,
        lock_written: bool,
        trust: Option<TrustState>,
    ) -> Self {
        Self {
            cwd: plan.cwd().display().to_string(),
            shell: spec.kind.as_str(),
            exec: spec.exec.clone(),
            depth: plan.depth(),
            path_before: plan.path_before().to_owned(),
            path_after: plan.path_after().to_owned(),
            prepend: plan
                .prepend()
                .iter()
                .map(|dir| dir.display().to_string())
                .collect(),
            tools: tools_view(plan.tools()),
            warnings: plan.warnings().iter().map(WarningView::from).collect(),
            lock_written,
            trust: trust.map(|state| state.as_str()),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// `trust`
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen trust --list --json` 的成功载荷。
#[derive(Debug, Serialize)]
pub struct TrustListView {
    /// 信任清单自己的位置。**必须报出来**：用户唯一的查看入口，
    /// 而"它在哪"决定了这份清单能不能被备份、能不能被企业策略覆盖（决策 17）。
    #[serde(rename = "trustFile")]
    pub trust_file: String,
    /// 每一条信任记录，**连同它现在还有不有效**。
    pub entries: Vec<TrustEntryView>,
}

/// 一条信任记录 + **它现在**的状态。
///
/// 重算状态是刻意的（决策 118）：一个把过期条目显示成"已信任"的清单是在说谎，
/// 而它恰好是用户唯一的查看入口。
#[derive(Debug, Serialize)]
pub struct TrustEntryView {
    /// 被信任的目录（绝对路径）。
    pub path: String,
    /// 首次信任时的指纹（`sha256:<hex>`）。
    pub fingerprint: String,
    /// 什么时候信任的（RFC3339 UTC）。
    #[serde(rename = "trustedAt")]
    pub trusted_at: String,
    /// **现在**的状态：`trusted` / `stale` / `missing-file`。
    ///
    /// 注意这里**不会**出现 `not-trusted`：清单里的每一条都是"信任过"的记录，
    /// 而 `not-trusted` 说的是"这个目录根本不在清单里" —— 那是 `auto` 的判据，
    /// 不是 `--list` 的。一个在 `--list` 里永远不出现的取值不该混进这份枚举。
    pub state: &'static str,
}

impl TrustEntryView {
    /// 一条记录 + 它**现在**的状态。`state` 由调用方**现场重算**后传进来 ——
    /// 这一层不自己算（它拿不到磁盘），也不缓存（缓存会让"过期"永远看不见）。
    #[must_use]
    pub fn new(entry: &TrustEntry, state: TrustState) -> Self {
        Self {
            path: entry.path.clone(),
            fingerprint: entry.fingerprint.clone(),
            trusted_at: entry.trusted_at.clone(),
            state: state.as_str(),
        }
    }
}

/// `tuoen trust`（无参数，信任当前目录）的成功载荷。
#[derive(Debug, Serialize)]
pub struct TrustAddView {
    /// 被信任的目录（绝对化之后的）。
    pub path: String,
    /// 刚算出来的指纹。
    pub fingerprint: String,
    /// 恒为 `"trusted"`。**这个键存在的理由**：`--revoke` 的载荷里有一个
    /// `action`，两个命令的载荷形状因此可以对着读；而且将来若加
    /// "已经信任过、指纹没变"这种结果，它有一个现成的位置。
    pub action: &'static str,
}

/// `tuoen trust --revoke <PATH>` 的成功载荷。
#[derive(Debug, Serialize)]
pub struct TrustRevokeView {
    /// 被摘掉的（或本来就不在的）路径，**原样回报调用方给的那条**。
    pub path: String,
    /// `revoked` = 真的摘掉了一条；`absent` = 清单里本来就没有它。
    ///
    /// **`absent` 不是失败**：撤销的目标状态（"这个目录不在清单里"）已经成立。
    /// 但它**也不是 `revoked`** —— 把两者合并就是"假装成功"，
    /// 而用户最需要知道的恰好是"我摘的那条到底在不在"。
    pub action: &'static str,
}

// ─────────────────────────────────────────────────────────────────────────────
// `lock`
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen lock --json` 的成功载荷。
#[derive(Debug, Serialize)]
pub struct LockView {
    /// 锁文件的位置。
    #[serde(rename = "lockFile")]
    pub lock_file: String,
    /// 这一次**真的写了**吗（`--dry-run` 恒为 `false`）。
    pub written: bool,
    /// 解析出来的每个工具。
    pub tools: Vec<ToolView>,
}

impl LockView {
    /// 从解析结果与锁文件位置组装。
    #[must_use]
    pub fn new(lock_file: &Path, tools: &[ResolvedTool], written: bool) -> Self {
        Self {
            lock_file: lock_file.display().to_string(),
            written,
            tools: tools_view(tools),
        }
    }

    /// 从**已经读回来的**锁文件组装（`lock` 写完之后的复核用）。
    ///
    /// 存在理由：`LockFile::to_toml()` 是纯函数，但"写下去再读回来"是**两件事**。
    /// 这条路径让 `lock` 能报"磁盘上现在是什么"，而不是"我以为我写了什么"。
    #[must_use]
    pub fn from_lock_file(lock_file: &Path, lock: &LockFile, written: bool) -> Self {
        Self {
            lock_file: lock_file.display().to_string(),
            written,
            tools: lock
                .tool
                .iter()
                .map(|tool| ToolView {
                    name: tool.name.clone(),
                    spec: tool.spec.clone(),
                    version: tool.version.clone(),
                    source: tool.source.clone(),
                    manager: tool.manager.clone(),
                    path: tool.path.clone(),
                })
                .collect(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 失败形状（四个命令共用）
// ─────────────────────────────────────────────────────────────────────────────

/// 一次失败：一个稳定的机器可读码 + 一句中文（含下一步该跑什么命令）。
///
/// `code` 是 `&'static str` 而不是 `String`：**它必须来自一个常量**。
/// 一个运行时拼出来的错误码意味着存在一条没有名字的错误路径 ——
/// 而脚本要按这个 slug 分支，没有名字的那条分支没人能写。
#[derive(Debug)]
pub struct Failure {
    /// 稳定的小写 kebab ASCII 码（`missing-pin` / `lock-mismatch` / `untrusted` …）。
    pub code: &'static str,
    /// 中文人话。**不进 `--json` 的 `data`**，只在 `error.message` 与 stderr 上。
    pub message: String,
}

impl From<PinError> for Failure {
    fn from(error: PinError) -> Self {
        // **码与消息都取自 core**：`code()` 是稳定 slug，`message()` 是中文人话
        // （含下一步命令）。在这一层再拼一遍的结果是两处对同一条错误说两句不同的话。
        Self {
            code: error.code(),
            message: error.message(),
        }
    }
}

impl Failure {
    /// 一次本地的 I/O 失败。**码取自 core**（`PinError::Io` 的 slug）。
    ///
    /// 走 core 而不是在这一层写一个字面量 `"pin-io"`：同一个 slug 有两个来源时，
    /// 某一天 core 改了拼法，改到的那一半会让脚本按一个**永远不会出现**的码分支。
    /// 消息由调用方给 —— core 不知道我们当时在读哪个目录。
    ///
    /// `path` 为 `None` 用于"读不出当前目录"这类**没有具体文件**的失败：
    /// core 的 `PinError::Io` 强制要求一个路径，而往那里填一个编出来的路径
    /// 只会让消息变成一句看起来很合理的错话（`AGENTS.md` 里最危险的那一类）。
    #[must_use]
    pub fn io(path: Option<&Path>, message: impl Into<String>) -> Self {
        // 这个实例只用来读常量 `code()`，它的路径与消息都不参与输出。
        let code = PinError::Io {
            path: PathBuf::new(),
            message: String::new(),
        }
        .code();
        match path {
            Some(path) => PinError::Io {
                path: path.to_path_buf(),
                message: message.into(),
            }
            .into(),
            None => Self {
                code,
                message: message.into(),
            },
        }
    }

    /// 一次 **CLI 自己拥有**的失败。
    ///
    /// `code` 必须是常量（见 [`Failure::code`] 的文档）：`untrusted` /
    /// `fingerprint-mismatch` / `unwired-root` 三个码不在 core 里，因为它们判的
    /// 不是"pin 文件怎么了"，而是"这一次要不要自动应用它"。
    #[must_use]
    pub fn cli(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// 这个目录**不在**信任清单里（决策 15：自动切换要 `trust` 一次）。
///
/// **core 不拥有这个码**：`PinError` 的八个码里没有它 —— 它判的不是"pin 文件怎么了"，
/// 而是"这个目录被信任了吗"，那是 `auto` 独有的那道门。
pub const UNTRUSTED: &str = "untrusted";

/// 信任过，但 `tuoen.toml` 的指纹变了（决策 117/121）。
///
/// 与 [`UNTRUSTED`] 分开是刻意的：两者的**原因**不同（"没信任过" vs "内容变了"），
/// 而把它报成 `untrusted` 会让"我明明信任过"变成一句用户无法反驳的错话。
pub const FINGERPRINT_MISMATCH: &str = "fingerprint-mismatch";

/// 算不出信任清单的位置（`%APPDATA%` 读不到）。
///
/// **与 core 的 `PinError::code()` 同值**（`unwired-root`）—— 它是同一个判据，
/// 但这一处是 CLI 自己判出来的：core 的 `TrustFile::at_default_location()` 在
/// `%APPDATA%` 读不到时只是返回 `None`，它**没有**一个错误可给
/// （core 的 `PinError::UnwiredRoot` 说的是**机器级策略**那条根，不是这一条）。
/// 同一条思路见 `doctor` 的 `unwired-roots`（决策 109）：宁可失败，
/// 不要往一个随 cwd 变的地方写信任清单（决策 16：信任记录必须在被信任的目录**之外**）。
pub const UNWIRED_ROOT: &str = "unwired-root";

/// 报一次失败。`--json` 时信封走 stdout，否则（以及 `also_stderr` 时）中文走 stderr。
///
/// `also_stderr` 用于 `auto` 的那道信任门：决策 121 明确要求"未信任 → 不启动 +
/// **stderr** 提示"，所以那条路上即使 `--json` 也往 stderr 写一句 ——
/// 信封在 stdout、提示在 stderr，两者互不污染。
pub fn report(command: &'static str, json: bool, failure: &Failure, also_stderr: bool) -> i32 {
    if json {
        crate::print_json(&Envelope::err(
            command,
            failure.code,
            failure.message.clone(),
        ));
    }
    if also_stderr || !json {
        eprintln!("tuoen: {}", failure.message);
    }
    exit::RUNTIME_ERROR
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

/// 把遮蔽警告打到 **stderr**。
///
/// 决策 120 要求"不静默继续"，而"不静默"的落点就在这里。
///
/// **为什么是 stderr 而不是 stdout**：`--json` 的载荷在 stdout 上，
/// 而警告天生是给**人**看的一句中文。混进 stdout 会让 JSON 契约当场破掉
/// （这正是决策 123 把 `--json` 与 `--dry-run` 绑在一起要防的那类事故）。
/// 交互式 shell 那条路上 stdout 还要被子进程用，更不该往里写。
pub fn print_warnings(warnings: &[ShadowWarning]) {
    if warnings.is_empty() {
        return;
    }
    eprintln!();
    eprintln!(
        "tuoen: 警告：有 {} 个命令**前置了却没生效** —— 你以为切了版本，其实没有。",
        warnings.len()
    );
    for warning in warnings {
        eprintln!("{}", warning_line(warning));
    }
    eprintln!(
        "  原因通常是：这个工具的版本目录里没有那个命令（布局猜错了），\
         或者 `PATH` 上另有一个更早的安装。"
    );
    eprintln!();
}

/// 一条警告 → 一句中文。
///
/// **纯函数**是刻意的：决策 120 要求"不静默继续"，而三种形态各印哪一句
/// 正是这一族里唯一必须逐字对的东西 —— 把它留在 `eprintln!` 里就没有任何
/// 办法钉住它，而印错的那一句恰好是用户唯一能读到的信息。
///
/// 三种形态（`winner` / `entry` 的组合）**不是同义反复**：
///
/// * `(Some, Some)` 且两者指同一个目录 → 只印 `entry`（`PATH` 里的**原文**，
///   用户能拿它一眼对回自己环境变量里的那一条）。
/// * `(Some, Some)` 且两者不同 → 两个都印，因为那时它们真的不是一回事。
/// * `(Some, None)` / `(None, _)` → 见下面那一句。
#[must_use]
fn warning_line(warning: &ShadowWarning) -> String {
    match (&warning.winner, &warning.entry) {
        // `winner` 是归一化后的目录文本、`entry` 是它在 `PATH` 里的原文
        // （可能带尾部分隔符或引号）—— 它们**指同一个目录**。
        // 不加这一支就会印出"抢到的是 A 里的 A"，那种话读起来像实现漏了一步，
        // 而它其实一个额外的字都没说。
        (Some(winner), Some(entry)) if same_path_text(winner, entry) => format!(
            "  · `{}` 抢到的是 `{}` —— 不是我们前置的目录。",
            warning.command, entry
        ),
        (Some(winner), Some(entry)) => format!(
            "  · `{}` 抢到的是 `{}`（`PATH` 里写的是 `{}`）—— 不是我们前置的目录。",
            warning.command, winner, entry
        ),
        (Some(winner), None) => format!(
            "  · `{}` 抢到的是 `{}` —— 不是我们前置的目录。",
            warning.command, winner
        ),
        // **`winner = None` 是单独可辨的一态**（决策 120）：不是"被别人抢了"，
        // 而是"我们前置的目录里根本没有这个命令"。两者的修法完全不同，
        // 所以这里印的是两句话，不是一句话加一个空值。
        (None, _) => format!(
            "  · `{}` 在整条 `PATH` 上都找不到 —— 前置的目录里没有它，\
             这台机器上也没有它。",
            warning.command
        ),
    }
}

/// 两段路径文本是不是指同一个目录（忽略大小写、引号与尾部分隔符）。
///
/// 与 core 的 `same_dir` 是同一条判据，但这里比的是**文本**不是 `Path`：
/// `winner` 是归一化文本、`entry` 是原文，两边都可能带着引号或尾分隔符，
/// 而它们必须被判成同一个目录。
fn same_path_text(left: &str, right: &str) -> bool {
    let trim = |text: &str| {
        text.trim()
            .trim_matches('"')
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    };
    trim(left) == trim(right)
}

/// `tuoen shell` / `tuoen auto` 的人类输出。
///
/// 三段：这一条计划是什么 → 生效的工具 → 这次碰了什么。
/// **最后一段（"碰了什么"）是刻意留的**：这一族唯一会写的东西是 `tuoen.lock`，
/// 而用户最需要一眼看到的就是"它写没写、写在哪"。
pub fn print_plan_human(view: &ShellPlanView, lock_file: &Path) {
    println!(
        "tuoen {} —— 计划（`--dry-run`：没有启动任何子进程）。",
        command_label(view)
    );
    println!();
    println!("目录：{}", view.cwd);
    match &view.exec {
        Some(_) => println!("shell：{}（非交互，跑一条命令）", view.shell),
        None => println!("shell：{}（交互式）", view.shell),
    }
    if let Some(command) = &view.exec {
        println!("命令：{command}");
    }
    if let Some(trust) = view.trust {
        println!("信任：{trust}");
    }
    println!("深度：{}", view.depth);
    println!();

    if view.tools.is_empty() {
        // 一个空的 pin 是**合法的**（`[tools]` 可以为空），但它很容易被误读成
        // "pin 生效了"。所以这里明说。
        println!("这个 `tuoen.toml` 没有 pin 任何工具（`[tools]` 是空的）。");
    } else {
        println!("前置目录（按工具 id 字典序）：");
        for dir in &view.prepend {
            println!("  {dir}");
        }
        println!();
        let name_w = view
            .tools
            .iter()
            .map(|tool| crate::display_width(&tool.name))
            .max()
            .unwrap_or(4)
            .max(4);
        let spec_w = view
            .tools
            .iter()
            .map(|tool| crate::display_width(&tool.spec))
            .max()
            .unwrap_or(4)
            .max(4);
        // 版本与来源这两列的宽度**从数据里算**，不写死：`path-resolution`
        // 是 15 列，写死 12 会让它把后面的路径列推歪。
        // 下限是表头本身的宽度（`生效版本` 8 列 / `来源` 4 列）。
        let version_w = view
            .tools
            .iter()
            .map(|tool| crate::display_width(&tool.version))
            .max()
            .unwrap_or(8)
            .max(8);
        let source_w = view
            .tools
            .iter()
            .map(|tool| crate::display_width(&tool.source))
            .max()
            .unwrap_or(4)
            .max(4);
        println!(
            "{}  {}  {}  {}  路径",
            pad("工具", name_w),
            pad("声明", spec_w),
            pad("生效版本", version_w),
            pad("来源", source_w),
        );
        for tool in &view.tools {
            let manager = tool.manager.as_deref().unwrap_or("");
            println!(
                "{}  {}  {}  {}  {}{}",
                pad(&tool.name, name_w),
                pad(&tool.spec, spec_w),
                pad(&tool.version, version_w),
                pad(&tool.source, source_w),
                tool.path,
                if manager.is_empty() {
                    String::new()
                } else {
                    format!("（由 {manager} 管理）")
                },
            );
        }
        println!();
    }

    if view.warnings.is_empty() {
        println!("遮蔽检查：pin 的每个命令都从前置目录里解析到了。");
    } else {
        // 警告本身已经打到 stderr 了（含命令名与赢家条目名）。这里只留一句
        // **指向**，因为一份"上面有警告、下面说一切正常"的报告比没有报告更糟。
        println!(
            "遮蔽检查：{} 个命令前置了却没生效 —— 详情在 stderr 上。",
            view.warnings.len()
        );
    }
    println!();

    if view.lock_written {
        println!("已写出锁文件：{}", lock_file.display());
    } else {
        println!(
            "没有写锁文件（`--dry-run`，或者锁已经与 `tuoen.toml` 一致）：{}",
            lock_file.display()
        );
    }
    println!();
    println!("（本次只读：没有翻转任何 junction、没有改父进程的环境、没有动任何工具。）");
    println!("  去掉 `--dry-run` 才会真的起 shell；`--json` 只描述计划（决策 123）。");
}

fn command_label(view: &ShellPlanView) -> &'static str {
    if view.trust.is_some() {
        "auto"
    } else {
        "shell"
    }
}

/// `tuoen trust --list` 的人类输出。
///
/// **每一条都要印出它现在的状态**，而且要在末尾说清三种状态各是什么意思 ——
/// 一个只印路径的清单会让人以为"在清单里 = 现在有效"，而那份清单的全部价值
/// 恰好在于它**区分**这两件事（决策 118）。
pub fn print_trust_list_human(view: &TrustListView) {
    println!("tuoen trust —— 信任清单");
    println!();
    println!("清单：{}", view.trust_file);
    println!();

    if view.entries.is_empty() {
        println!("（清单是空的 —— 还没有信任过任何目录）");
        println!();
        println!("信任当前目录：`tuoen trust`。");
        println!("信任之后，`tuoen auto` 才会在那个目录里自动应用 pin。");
        return;
    }

    let path_w = view
        .entries
        .iter()
        .map(|entry| crate::display_width(&entry.path))
        .max()
        .unwrap_or(4)
        .max(4);
    println!("{}  {}  指纹", pad("目录", path_w), pad("现在", 12));
    for entry in &view.entries {
        println!(
            "{}  {}  {}",
            pad(&entry.path, path_w),
            pad(entry.state, 12),
            short_fingerprint(&entry.fingerprint),
        );
    }
    println!();

    for (state, label) in [
        (
            "trusted",
            "指纹与信任时一致，`tuoen auto` 会应用这个目录的 pin。",
        ),
        (
            "stale",
            "`tuoen.toml` 变了（指纹对不上），`tuoen auto` 会**拒绝**执行 —— 重新 `tuoen trust` 才能恢复。",
        ),
        (
            "missing-file",
            "拿不到 `tuoen.toml` 的指纹（文件不在了、目录被搬走了、或者读不了），这条记录现在无法验证。",
        ),
    ] {
        let count = view
            .entries
            .iter()
            .filter(|entry| entry.state == state)
            .count();
        if count > 0 {
            println!("{count} 条 `{state}`：{label}");
        }
    }
    println!();
    println!("摘掉一条：`tuoen trust --revoke <目录>`。");
}

/// 指纹只印前 16 位十六进制：完整的那一串在 `--json` 里，而人读的是"变了没有"。
fn short_fingerprint(fingerprint: &str) -> String {
    match fingerprint.strip_prefix("sha256:") {
        Some(hex) if hex.len() > 16 => format!("sha256:{}…", &hex[..16]),
        _ => fingerprint.to_owned(),
    }
}

/// 把一个字符串补空格补到指定的**显示宽度**（CJK 占两列）。
///
/// **不能用 `{:<width$}`**：它按**字符数**补，而"工具"是两个字符、四个列宽 ——
/// 于是中文表头会比它下面的数据行短两列，整张表错开。列宽一律用
/// [`crate::display_width`] 算，所以补空格也必须用同一把尺子。
fn pad(text: &str, width: usize) -> String {
    let mut out = String::from(text);
    for _ in crate::display_width(text)..width {
        out.push(' ');
    }
    out
}

/// `tuoen trust`（信任当前目录）的人类输出。
pub fn print_trust_add_human(view: &TrustAddView, trust_file: &Path) {
    println!("已信任：{}", view.path);
    println!("指纹：{}", view.fingerprint);
    println!("清单：{}", trust_file.display());
    println!();
    println!("从这一刻起，在这个目录（以及指纹没变的它）里 `tuoen auto` 会应用 pin。");
    println!("`tuoen.toml` 改一个字符就会让指纹对不上，那时 `auto` 会拒绝执行并让你重新 trust。");
}

/// `tuoen trust --revoke` 的人类输出。
pub fn print_trust_revoke_human(view: &TrustRevokeView, trust_file: &Path) {
    match view.action {
        "revoked" => println!("已摘掉：{}", view.path),
        // **不印"已摘掉"**：清单里本来就没有它。把 no-op 印成成功就是"假装成功"，
        // 而用户问的正是"我摘的那条到底在不在"。
        _ => println!("清单里本来就没有这一条：{}", view.path),
    }
    println!("清单：{}", trust_file.display());
    if view.action == "revoked" {
        println!();
        println!("这个目录里的 `tuoen auto` 从现在起会拒绝执行（回到未信任）。");
    }
}

/// `tuoen lock` 的人类输出。
pub fn print_lock_human(view: &LockView) {
    if view.written {
        println!("已写出锁文件：{}", view.lock_file);
    } else {
        println!("锁文件（`--dry-run`：没有写）：{}", view.lock_file);
    }
    println!();
    if view.tools.is_empty() {
        println!("这个 `tuoen.toml` 没有 pin 任何工具（`[tools]` 是空的），锁里也没有工具。");
        return;
    }
    for tool in &view.tools {
        let manager = tool
            .manager
            .as_deref()
            .map(|name| format!("（由 {name} 管理）"))
            .unwrap_or_default();
        println!(
            "  {} {} → {}  [{}]  {}{}",
            tool.name, tool.spec, tool.version, tool.source, tool.path, manager
        );
    }
    println!();
    println!("锁文件是**解析结果**，不是声明：它进 git，用来发现");
    println!("「`tuoen.toml` 改了而锁没跟上」这件事 —— 那时 `shell` / `auto` 会拒绝启动。");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shadow_warning_never_repeats_the_same_directory_twice() {
        // `winner` 是归一化文本、`entry` 是 `PATH` 里的原文，两者**指同一个目录**。
        // 不加判据就会印出"抢到的是 A 里的 A" —— 一句读起来像实现漏了一步的话。
        let same = ShadowWarning {
            command: "python3".to_owned(),
            winner: Some(r"C:\Python312\Scripts".to_owned()),
            entry: Some(r"C:\Python312\Scripts\".to_owned()),
        };
        let line = warning_line(&same);
        assert!(!line.contains("里的"), "同一个目录不该印两遍：{line}");
        assert!(line.contains(r"C:\Python312\Scripts\"), "{line}");
        assert!(line.contains("python3"), "{line}");

        // 引号也是"原文"的一部分。
        let quoted = ShadowWarning {
            command: "node".to_owned(),
            winner: Some(r"C:\nvm4w\nodejs".to_owned()),
            entry: Some("\"C:\\nvm4w\\nodejs\"".to_owned()),
        };
        assert!(
            !warning_line(&quoted).contains("里的"),
            "{}",
            warning_line(&quoted)
        );

        // 真的不一样时两个都要印 —— 那时它们确实不是一回事。
        let different = ShadowWarning {
            command: "node".to_owned(),
            winner: Some(r"C:\nvm4w\nodejs".to_owned()),
            entry: Some(r"C:\other\nodejs".to_owned()),
        };
        let line = warning_line(&different);
        assert!(line.contains(r"C:\nvm4w\nodejs"), "{line}");
        assert!(line.contains(r"C:\other\nodejs"), "{line}");
    }

    #[test]
    fn the_no_winner_state_is_a_different_sentence() {
        // 决策 120：`winner = None` 是"整条 `PATH` 上都没有这个命令"，
        // 比"被压住"更糟，修法也不同 —— 它必须是另一句话。
        let missing = ShadowWarning {
            command: "node".to_owned(),
            winner: None,
            entry: None,
        };
        let line = warning_line(&missing);
        assert!(line.contains("找不到"), "{line}");
        assert!(!line.contains("抢到的是"), "{line}");

        // 只有 `winner` 没有 `entry` 是防御性的一支（core 现在不会造它），
        // 但它必须仍然印得出人话，而不是一个空目录。
        let half = ShadowWarning {
            command: "node".to_owned(),
            winner: Some(r"C:\nvm4w\nodejs".to_owned()),
            entry: None,
        };
        let line = warning_line(&half);
        assert!(line.contains(r"C:\nvm4w\nodejs"), "{line}");
        assert!(!line.contains("``"), "不许印出一个空的反引号对：{line}");
    }

    #[test]
    fn a_table_cell_is_padded_by_display_width_not_by_char_count() {
        // `{:<w$}` 按**字符数**补，而中文表头是"两个字符、四个列宽" ——
        // 用它补出来的表会让表头与它下面的数据行错开两列。这条用例钉住补空格
        // 用的那把尺子（与算列宽用的 `display_width` 是同一把）。
        assert_eq!(pad("工具", 8), "工具    ");
        assert_eq!(pad("node", 8), "node    ");
        assert_eq!(pad("目录", 4), "目录");
        assert_eq!(pad("node", 4), "node");
        // 已经超宽就不截断：截断一个路径比错位危险得多。
        assert_eq!(pad("工具", 2), "工具");
    }

    #[test]
    fn the_cli_level_error_codes_are_lowercase_kebab_ascii() {
        // 这三个码 core 不拥有（它们是 CLI 自己判出来的），所以必须在这里钉住。
        for code in [UNTRUSTED, FINGERPRINT_MISMATCH, UNWIRED_ROOT] {
            assert!(
                code.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "错误码必须是小写 kebab ASCII：{code}"
            );
        }
    }

    #[test]
    fn a_local_io_failure_takes_its_code_from_core() {
        // 这里**不许**出现一个字面量 `"pin-io"`：同一个 slug 有两个来源时，
        // 某一天 core 改了拼法，脚本会按一个永远不会出现的码分支。
        let with_path = Failure::io(Some(Path::new(r"C:\tmp\tuoen.lock")), "读不出来");
        assert_eq!(with_path.code, "pin-io");
        assert!(
            with_path.message.contains("tuoen.lock"),
            "core 的消息要带上路径：{}",
            with_path.message
        );

        // 没有具体文件时**不许编一个路径出来**。
        let bare = Failure::io(None, "读不出当前目录");
        assert_eq!(bare.code, "pin-io");
        assert_eq!(bare.message, "读不出当前目录");
    }

    fn has_cjk(text: &str) -> bool {
        text.chars()
            .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
    }

    fn a_tool() -> ResolvedTool {
        ResolvedTool {
            name: "node".to_owned(),
            spec: "24".to_owned(),
            version: "24.19.0".to_owned(),
            source: "tuoen".to_owned(),
            manager: None,
            path: std::path::PathBuf::from(r"C:\store\node\versions\24.19.0"),
            hash: None,
        }
    }

    #[test]
    fn a_tool_view_is_the_frozen_six_key_shape() {
        let view = ToolView::from(&a_tool());
        let json = serde_json::to_string(&view).expect("序列化");
        assert_eq!(
            json,
            r#"{"name":"node","spec":"24","version":"24.19.0","source":"tuoen","manager":null,"path":"C:\\store\\node\\versions\\24.19.0"}"#
        );
        assert!(!has_cjk(&json), "{json}");
        // `hash` **不进** `--json`：它是锁文件的字段（见模块文档）。
        assert!(!json.contains("hash"), "{json}");
    }

    #[test]
    fn a_manager_is_null_not_a_missing_key() {
        // 第一类可选字段：`null` 是"这个工具不由任何第三方管理器管"这个**真实的答案**。
        let mut tool = a_tool();
        tool.manager = Some("nvm4w".to_owned());
        let json = serde_json::to_string(&ToolView::from(&tool)).expect("序列化");
        assert!(json.contains(r#""manager":"nvm4w""#), "{json}");
    }

    #[test]
    fn a_warning_keeps_the_no_winner_state_distinguishable() {
        // 决策 120：`winner = None`（前置了但整条 PATH 上都没有这个命令）
        // 与"被别人抢了"是**两态**，不许合并成空串。
        let no_winner = WarningView::from(&ShadowWarning {
            command: "node".to_owned(),
            winner: None,
            entry: None,
        });
        let json = serde_json::to_string(&no_winner).expect("序列化");
        assert_eq!(json, r#"{"command":"node","winner":null,"entry":null}"#);

        let shadowed = WarningView::from(&ShadowWarning {
            command: "node".to_owned(),
            winner: Some(r"C:\nvm4w\nodejs".to_owned()),
            entry: Some("node.exe".to_owned()),
        });
        let json = serde_json::to_string(&shadowed).expect("序列化");
        assert_eq!(
            json,
            r#"{"command":"node","winner":"C:\\nvm4w\\nodejs","entry":"node.exe"}"#
        );
        assert!(!has_cjk(&json), "{json}");
    }

    #[test]
    fn trust_entries_render_with_camel_case_keys_and_a_stable_state_slug() {
        let view = TrustEntryView::new(
            &TrustEntry {
                path: r"C:\Work\proj".to_owned(),
                fingerprint: "sha256:00".to_owned(),
                trusted_at: "2026-10-02T12:00:00Z".to_owned(),
            },
            TrustState::Stale {
                expected: "sha256:00".to_owned(),
                actual: "sha256:11".to_owned(),
            },
        );
        let json = serde_json::to_string(&view).expect("序列化");
        assert_eq!(
            json,
            r#"{"path":"C:\\Work\\proj","fingerprint":"sha256:00","trustedAt":"2026-10-02T12:00:00Z","state":"stale"}"#
        );
        assert!(!has_cjk(&json), "{json}");
    }

    #[test]
    fn the_three_trust_states_have_their_documented_slugs() {
        for (state, slug) in [
            (TrustState::Trusted, "trusted"),
            (
                TrustState::Stale {
                    expected: "sha256:00".to_owned(),
                    actual: "sha256:11".to_owned(),
                },
                "stale",
            ),
            (TrustState::MissingFile, "missing-file"),
            (TrustState::NotTrusted, "not-trusted"),
        ] {
            assert_eq!(state.as_str(), slug);
        }
    }

    #[test]
    fn the_short_fingerprint_never_invents_a_value() {
        assert_eq!(
            short_fingerprint("sha256:0123456789abcdef0123"),
            "sha256:0123456789abcdef…"
        );
        // 短得出奇的输入**原样印**，不截断也不补零 —— 猜一个比不印更糟。
        assert_eq!(short_fingerprint("sha256:abc"), "sha256:abc");
        assert_eq!(short_fingerprint(""), "");
    }
}
