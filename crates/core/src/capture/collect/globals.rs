//! `globals.toml` —— 全局包清单（决策 167–175）。
//!
//! 这一节只回答一个问题：**这台机器上有哪些全局包**。答案**只来自工具自己的回答**
//! （决策 167），我们不自己走 `<prefix>\node_modules` —— 实测那个目录只数到 5 个
//! 而 `npm ls -g` 说 7 个，少掉的两个在迁移时表现为"新机器上少装了两个"。
//!
//! # 命令表只有一处
//!
//! 七条命令里有五条属于这一节（另外两条是 `configs` 的 git），全部是 [`Command`]
//! 常量：`program` 是**裸名字**（决策 168），参数是编译期常量、零插值零引号。
//! 调用点只引用常量 —— 固定装置按 `program` + 参数前缀匹配（决策 185），
//! 命令字符串散落两处就会对不上，而对不上表现为"工具不可用/枚举失败"。
//!
//! # 两个超时是分开的（决策 170）
//!
//! * **`--version` 探测**用 `ctx.probe_timeout`（平台的 3 秒）：它只启动一个解释器；
//! * **要启动包管理器的命令**用 [`GLOBALS_TIMEOUT`]（60 秒）：`npm ls -g` 实测冷
//!   **12.4 s**、热 0.77 s，`npm config get prefix` 同样要启动 node 并读配置链。
//!   用探测超时会把冷启动判成"枚举失败" —— 那是**假阴性**，而假阴性在这里等于
//!   "新机器上少了几个全局包"。
//!
//! # 枚举失败不是错误（决策 175）
//!
//! 超时、非零退出、输出不是 JSON、JSON 形状不认识 —— 四种都**照样写出这一行**，
//! 带着 prefix 与 tool_version，只在 `enumerate_error` 里给出具体的 slug。
//! "发现了这个全局前缀，但数不出里面的包"是一个有用的结果；而"这一节什么都没有"
//! 是一句假话。四种失败也**不走 `skipped.toml`**：同一个事实写两处就是第二份真相。
//!
//! # 两个来源（ticket #23）
//!
//! 决策 167 说的是"只信工具自己的回答"，而**问哪个工具**有两份答案：
//!
//! * `source = "machine"` —— 用户当下真正在用的那套全局位置（上面全部规则的原样）；
//! * `source = "tuoen"` —— `%LOCALAPPDATA%\tuoen\globals\` 那个由我们按运行时版本
//!   隔离出来的根（决策 26）。它靠**只改子进程环境**读到：`NPM_CONFIG_PREFIX`
//!   （npm）与 `PYTHONUSERBASE` + `PIP_USER=1`（pip），**绝不写 `.npmrc`/`pip.ini`**
//!   （决策 27）。
//!
//! 同一个工具的两份清单是**两行**（决策 166 的加法变更：`globals.toml` 多一个
//! 永远出键的 `source`）。合并成一行会丢掉"这个包该装进哪个根"这个信息，
//! 而 `restore` 正是靠它决定往哪儿装的。
//!
//! tuoen 那一行**只在真的有事可说时**才出现：根不存在、根里没有包、也没有枚举
//! 失败 —— 三种都是"这里什么都没有"，那是 `roots`（`globals list` 的 `roots` 数组）
//! 要说的事，不是一行清单。反过来说：**给空根编一行是假话**，而"根在哪里、它是空的"
//! 必须说得出来（见 [`crate::globals`]）。

use std::path::Path;

use crate::detect::DetectContext;
use crate::globals::{
    GlobalsNote, GlobalsNoteCode, GlobalsRoot, GlobalsRootRow, GlobalsSource, GlobalsTool,
    root_from_context,
};

use super::super::files::{GlobalPackage, GlobalRow, GlobalsFile};
use super::{COMMAND_TIMEOUT, Command, find_on_path, first_line, has_version_segment};

/// 枚举全局包的超时（决策 170）。
///
/// **与 `probe_timeout` 分开**：后者是为 `--version` 定的几秒，而 `npm ls -g` 实测冷
/// **12.4 s**、热 0.77 s。用探测超时会把它判成"枚举失败"——那是**假阴性**，
/// 而假阴性在这里等于"新机器上少了几个全局包"。
pub(crate) const GLOBALS_TIMEOUT: std::time::Duration = COMMAND_TIMEOUT;

/// 判不出工具版本时写进 `tool_version` 的值（决策 172：**永远出键**，值可以是这个 slug）。
///
/// **它就是 [`crate::globals::UNKNOWN_VERSION`]**，不是另一个同值的字面量：
/// `tuoen shell` 那一侧要用同一个字符串判"这个版本能不能当目录名"，
/// 两处各写一遍迟早会漂移，而漂移的表现是"快照说 `unknown`、shell 说 `none`"。
const UNKNOWN: &str = crate::globals::UNKNOWN_VERSION;

/// npm 的**命令名**：在进程 `%Path%` 上找的是它（决策 171：找不到就不产生行）。
///
/// 工具名（`"npm"`）**不在这里**：它是 [`GlobalsTool::slug`] 的事 ——
/// 同一个字符串出现两处迟早会漂移成 `"npm "` 与 `"npm"`。
const NPM_COMMAND: &str = "npm.cmd";
/// `node -v` —— npm 行的 `tool_version` 来自 **node**（决策 172：本机 `v24.19.0`）。
///
/// 为什么不是 `npm -v`：`tool_version` 是"这份清单属于哪个**运行时**版本"，
/// 而 npm 自己的版本跟着 node 走、与包集合无关。
const NODE_VERSION: Command = Command {
    program: "node.exe",
    args: &["-v"],
};

/// npm 的全局前缀。
///
/// **`cmd.exe /C` 的形状是决策 168 定的**：`npm` 在 Windows 上只有 `npm.cmd`，
/// 直接 spawn 一个 `.cmd` 不是"启动一个程序"而是"启动 cmd 去解释一个脚本"。
const NPM_PREFIX: Command = Command {
    program: "cmd.exe",
    args: &["/C", "npm.cmd", "config", "get", "prefix"],
};

/// npm 的全局包清单。
///
/// `--offline` 是必需的（决策 169）：不加它，一个配了换源但连不上的机器会让这条命令
/// 等满超时；加了之后 npm 立刻答 `ENOTCACHED` 并**非零退出** —— 那落到
/// `command-failed`，是一个"发现但无法枚举"的正确答案。
const NPM_PACKAGES: Command = Command {
    program: "cmd.exe",
    args: &[
        "/C",
        "npm.cmd",
        "ls",
        "-g",
        "--json",
        "--depth=0",
        "--offline",
    ],
};

/// pip 的命令名。
///
/// **必须是 `pip.exe`**（或 `…\python.exe -m pip`），**绝不许**是 `python`：
/// 本机 `where python` 的第一条是
/// `C:\Users\Muelsyse\AppData\Local\Microsoft\WindowsApps\python.exe` ——
/// 一个 0 字节的 App Execution Alias，执行它拿到的是应用商店，不是 Python。
const PIP_COMMAND: &str = "pip.exe";

/// `pip --version` —— pip 行的 `tool_version`（里面的 python 版本）与 `prefix` **都**来自它。
///
/// 所以它**同时**是两个东西的来源：这就是为什么 `--no-version` 会让 pip 行连
/// `prefix` 一起失去（决策 174 的"同生共死"正好覆盖这个后果：两个键一起不出，
/// 而不是印一个 `prefix_inside_version_dir = false` 那样的假话）。
const PIP_VERSION: Command = Command {
    program: "pip.exe",
    args: &["--version"],
};

/// pip 的全局包清单。`--disable-pip-version-check` 挡掉那条"有新版本"的提示，
/// 它是**写 stdout 还是 stderr** 在不同 pip 版本里不一样 —— 与其猜，不如关掉它。
const PIP_PACKAGES: Command = Command {
    program: "pip.exe",
    args: &["list", "--format=json", "--disable-pip-version-check"],
};

/// npm **在我们自己的根里**的包清单（ticket #23）。
///
/// 与 [`NPM_PACKAGES`] 只差末尾的 `--prefix`：后面的值（那个根）是运行时才知道的，
/// 由调用点作为**追加参数**传进来（[`Command::run_with`]）。固定开关留在常量表里，
/// 常量表之外只出现"值"。
///
/// **实测**：`--prefix` 真的生效（不加它时看不到那棵树，只会看到机器自己那 7 个）。
/// 同一件事还有第二条路——`NPM_CONFIG_PREFIX` 环境变量，我们**两条都走**：
/// 命令行开关决定"读哪个树"，环境变量决定"npm 自己认为前缀在哪"（它还会影响
/// `npm ls` 内部对 prefix 的规范化）。两条同值，一条写错另一条还在。
const NPM_PACKAGES_IN_PREFIX: Command = Command {
    program: "cmd.exe",
    args: &[
        "/C",
        "npm.cmd",
        "ls",
        "-g",
        "--json",
        "--depth=0",
        "--offline",
        "--prefix",
    ],
};

/// pip 的**用户级**包清单（ticket #23）。配合 `PYTHONUSERBASE` + `PIP_USER=1`
/// （[`GlobalsRoot::redirect_env`]）才指向我们的根。
///
/// **实测**：重定向之后正好只列 tuoen 管的那些；不重定向时是空 `[]`
/// （本机 `python -m site --user-base` 指向一个不存在的 `…\AppData\Roaming\Python`）。
///
/// 用 `pip.exe` 这个名字（不是 `python`）：本机 `where python` 的第一条是
/// `…\WindowsApps\python.exe` —— 一个 0 字节的 App Execution Alias，执行它拿到的是
/// 应用商店，不是 Python。`find_on_path` 本来就跳过别名，但**换名字这条路根本不走**。
const PIP_USER_PACKAGES: Command = Command {
    program: "pip.exe",
    args: &[
        "list",
        "--user",
        "--format=json",
        "--disable-pip-version-check",
    ],
};

/// `enumerate_error` 的四个稳定 slug（决策 175）。
const COMMAND_FAILED: &str = "command-failed";
const TIMED_OUT: &str = "timed-out";
const BAD_JSON: &str = "bad-json";
const UNSUPPORTED_OUTPUT: &str = "unsupported-output";

/// 一行（已经枚举出来、还没落到 `globals.toml` 上）。
///
/// `tool` 与 `source` 是**枚举**而不是字符串：`globals.toml` 要 slug、`--json` 要 slug、
/// 人的表格要中文名 —— 三个消费者，而"拼字符串"的地方只剩 [`CollectedRow::to_file_row`]
/// 一处。更重要的：`source` 是枚举就意味着**写不出第三个数**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CollectedRow {
    /// 哪个工具。
    pub(crate) tool: GlobalsTool,
    /// 这一行是**谁的**清单（机器自己的 / 我们管的）。
    pub(crate) source: GlobalsSource,
    /// 运行时版本（`v24.19.0` / `3.12` / `unknown`）。
    pub(crate) tool_version: String,
    /// 工具报告的全局前缀（拿不到就是 `None`）。
    pub(crate) prefix: Option<String>,
    /// 前缀是不是落在按版本隔离的目录里（决策 173 的四支判据）。
    pub(crate) prefix_inside_version_dir: Option<bool>,
    /// 包清单（按名字排序）。
    pub(crate) packages: Vec<GlobalPackage>,
    /// 数不出包时的具体 slug（决策 175）；与 `packages` 不会同时有内容。
    pub(crate) enumerate_error: Option<String>,
}

impl CollectedRow {
    /// 落到 `globals.toml` 的那一行。
    ///
    /// `source` 是**永远出键**的字符串（决策 166：加一个键是加法变更，
    /// 不递增 `schemaVersion`）—— 所以它不是 `Option`，也没有"这一行不知道来源"
    /// 这种形态。
    pub(crate) fn to_file_row(&self) -> GlobalRow {
        GlobalRow {
            tool: self.tool.slug().to_owned(),
            source: self.source.slug().to_owned(),
            tool_version: self.tool_version.clone(),
            prefix: self.prefix.clone(),
            prefix_inside_version_dir: self.prefix_inside_version_dir,
            packages: self.packages.clone(),
            enumerate_error: self.enumerate_error.clone(),
        }
    }
}

/// 一次枚举的全部结果：行 + **我们管着的根** + "为什么这里什么都没有"。
///
/// 后两样是给 `tuoen globals list` 的（`capture` 只取 `rows`）：`capture` 的
/// `globals.toml` 是快照，而 `roots`/`notes` 回答的是"根在哪里、它是空的吗、
/// 为什么这个工具一行都没有" —— 那三个问题**都不是包清单**能回答的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CollectedGlobals {
    /// 两个来源的行，按"工具表顺序 + 来源顺序"。
    pub(crate) rows: Vec<CollectedRow>,
    /// 我们管着的根（空根也在）。
    pub(crate) roots: Vec<GlobalsRootRow>,
    /// 那些"什么都没出"的原因。
    pub(crate) notes: Vec<GlobalsNote>,
}

/// 采集全局包清单（`capture` 的入口）。
///
/// 行的顺序固定：npm 在前、pip 在后；同一个工具内部"机器自己的"在前、"tuoen 的"在后
/// （工具表的顺序是这一节的一部分，不按机器状态变）。
pub(crate) fn collect_globals(ctx: &DetectContext<'_>, captured_at: &str) -> GlobalsFile {
    // 根算不出来（进程环境里没有 `LOCALAPPDATA`，决策 187）时**照旧采机器那一份**：
    // `globals` 这一节从 #17 起就存在，一个环境变量缺失不许让它整节消失。
    // （`tuoen globals list` 是另一回事：那条命令的一半答案就是根，所以那里是错误。）
    let root = root_from_context(ctx).ok();
    let collected = collect_rows(ctx, root.as_ref());

    let mut file = GlobalsFile::new(captured_at);
    file.global = collected
        .rows
        .iter()
        .map(CollectedRow::to_file_row)
        .collect();
    file
}

/// 两个来源一起枚举。`root` 是**我们自己的**根（`None` = 算不出来，那就只采机器那一份）。
///
/// **根不存在时不起任何进程**：一台还没用过 tuoen 的机器上，`npm ls --prefix …` 与
/// `pip list --user` 都得不到有意义的答案，而它们要启动包管理器（冷启动十几秒）。
/// 这是铁律 3 在只读代码上的形态：先算"这一问值不值得问"。
pub(crate) fn collect_rows(
    ctx: &DetectContext<'_>,
    root: Option<&GlobalsRoot>,
) -> CollectedGlobals {
    let mut rows = Vec::new();
    let mut roots = Vec::new();
    let mut notes = Vec::new();

    // 工具表的顺序在这一处：npm 在前、pip 在后（决策 171）。
    for report in [npm_side(ctx, root), pip_side(ctx, root)] {
        rows.extend(report.rows);
        roots.extend(report.root);
        notes.extend(report.note);
    }

    CollectedGlobals { rows, roots, notes }
}

/// 一个工具的枚举结果：行 + 它的根（如果我们管着它的话）+ "为什么这里什么都没有"。
#[derive(Debug, Default)]
struct ToolReport {
    rows: Vec<CollectedRow>,
    root: Option<GlobalsRootRow>,
    note: Option<GlobalsNote>,
}

/// npm 那一侧：机器自己的那一行 + 我们根里的那一行。
fn npm_side(ctx: &DetectContext<'_>, root: Option<&GlobalsRoot>) -> ToolReport {
    let mut report = ToolReport::default();

    // 决策 171：可执行文件不在 `PATH` 上就**不产生行**，而且一次进程都不起
    //（`npm config get prefix` 也要启动 node）。
    if find_on_path(ctx, NPM_COMMAND).is_none() {
        report.note = Some(GlobalsNote {
            tool: GlobalsTool::Npm,
            code: GlobalsNoteCode::ToolNotOnPath,
        });
        return report;
    }

    let tool_version = probed_version(ctx, &NODE_VERSION);
    let prefix = probed_prefix(ctx, &NPM_PREFIX);
    let (packages, enumerate_error) = enumerate(ctx, &NPM_PACKAGES, &[], &[], parse_npm_packages);
    report.rows.push(CollectedRow {
        tool: GlobalsTool::Npm,
        source: GlobalsSource::Machine,
        tool_version: tool_version.clone(),
        prefix_inside_version_dir: prefix
            .as_deref()
            .map(|prefix| inside_version_dir(ctx, prefix)),
        prefix,
        packages,
        enumerate_error,
    });

    let Some(root) = root else { return report };
    // 我们的 npm 根 = `<base>\npm\<node -v 原样>`：**版本判不出来就没有这个根**
    // （决策 26 的按版本隔离 + 决策 172 的 `unknown`）。
    let Some(prefix) = root.npm_prefix(&tool_version) else {
        report.note = Some(GlobalsNote {
            tool: GlobalsTool::Npm,
            code: GlobalsNoteCode::RuntimeVersionUnknown,
        });
        return report;
    };
    report.root = Some(root_row(ctx, GlobalsTool::Npm, &prefix));
    if !ctx.fs.inspect(&prefix).exists {
        return report;
    }

    let prefix_value = prefix.to_string_lossy().into_owned();
    let env = root.redirect_env(GlobalsTool::Npm, &tool_version);
    // 两条路通向同一个地方是设计的一部分（一条写错另一条还在），但如果它们指向
    // **不同**的目录，读到的树与 npm 自己认为的前缀就不是一回事 —— 那会造出一份
    // "包在这个前缀里"的假清单。这个断言就是"它们一致"的机器检查。
    debug_assert_eq!(
        env.first().map(|(_, value)| value.as_str()),
        Some(prefix_value.as_str()),
        "`NPM_CONFIG_PREFIX` 与 `--prefix` 必须是同一个值"
    );

    let (packages, enumerate_error) = enumerate(
        ctx,
        &NPM_PACKAGES_IN_PREFIX,
        std::slice::from_ref(&prefix_value),
        &env,
        parse_npm_packages,
    );
    // 根存在、但没有包、也没有枚举失败 —— "这里什么都没有"是 `roots` 要说的事，
    // 不是一行清单（给空根编一行会让"tuoen 管着 0 个包"与"tuoen 管着这些包"
    // 在清单里长得一模一样）。
    if !packages.is_empty() || enumerate_error.is_some() {
        report.rows.push(CollectedRow {
            tool: GlobalsTool::Npm,
            source: GlobalsSource::Tuoen,
            tool_version,
            prefix_inside_version_dir: Some(inside_version_dir(ctx, &prefix_value)),
            prefix: Some(prefix_value),
            packages,
            enumerate_error,
        });
    }
    report
}

/// pip 那一侧：机器自己的那一行 + 我们根里的那一行。
fn pip_side(ctx: &DetectContext<'_>, root: Option<&GlobalsRoot>) -> ToolReport {
    let mut report = ToolReport::default();

    if find_on_path(ctx, PIP_COMMAND).is_none() {
        report.note = Some(GlobalsNote {
            tool: GlobalsTool::Pip,
            code: GlobalsNoteCode::ToolNotOnPath,
        });
        return report;
    }

    // `pip --version` **同时**是两个东西的来源：`tool_version`（括号里的 python 版本）
    // 与机器自己的 `prefix`（安装路径的形状）—— 这就是决策 174 的"同生共死"。
    // `--no-version` 关掉的就是这条命令（它按名字就是一个版本探测）。
    let answer = probed(ctx, &PIP_VERSION);
    let tool_version = answer
        .as_deref()
        .and_then(python_version)
        .unwrap_or_else(|| UNKNOWN.to_owned());
    let prefix = answer.as_deref().and_then(pip_prefix);
    let (packages, enumerate_error) = enumerate(ctx, &PIP_PACKAGES, &[], &[], parse_pip_packages);
    report.rows.push(CollectedRow {
        tool: GlobalsTool::Pip,
        source: GlobalsSource::Machine,
        tool_version: tool_version.clone(),
        prefix_inside_version_dir: prefix
            .as_deref()
            .map(|prefix| inside_version_dir(ctx, prefix)),
        prefix,
        packages,
        enumerate_error,
    });

    let Some(root) = root else { return report };
    // pip 的根**与版本无关**：`PYTHONUSERBASE` 指向 `<base>\pip`，`Python312\` 那一层
    // 是 **pip 自己**插的（实测）。所以 `--no-version` 之下这一半照旧成立 ——
    // 版本只影响 `binNames`（要 `Python312` 那个目录名），不影响包清单。
    let userbase = root.pip_userbase();
    report.root = Some(root_row(ctx, GlobalsTool::Pip, &userbase));
    if !ctx.fs.inspect(&userbase).exists {
        return report;
    }

    let env = root.redirect_env(GlobalsTool::Pip, &tool_version);
    let (packages, enumerate_error) =
        enumerate(ctx, &PIP_USER_PACKAGES, &[], &env, parse_pip_packages);
    if !packages.is_empty() || enumerate_error.is_some() {
        let userbase_value = userbase.to_string_lossy().into_owned();
        report.rows.push(CollectedRow {
            tool: GlobalsTool::Pip,
            source: GlobalsSource::Tuoen,
            tool_version,
            prefix_inside_version_dir: Some(inside_version_dir(ctx, &userbase_value)),
            prefix: Some(userbase_value),
            packages,
            enumerate_error,
        });
    }
    report
}

/// 一个根那一行。**"这个目录今天在不在"是看一眼磁盘的事**，不是包数能推出来的：
/// 空的根与不存在的根都有零个包，而它们对用户是两件事。
fn root_row(ctx: &DetectContext<'_>, tool: GlobalsTool, root: &Path) -> GlobalsRootRow {
    GlobalsRootRow {
        tool,
        root: root.to_path_buf(),
        source: GlobalsSource::Tuoen,
        exists: ctx.fs.inspect(root).exists,
    }
}

/// 跑一条 `--version` 探测并取第一行；答不上来（没启动/超时/非零退出/空）就是 [`UNKNOWN`]。
///
/// `ctx.probe_versions == false`（`--no-version`）时**一次进程都不起**：
/// 那个开关的整个意义就是"别为了版本号启动进程"（`cli.rs`：跳过版本探测，快得多）。
fn probed_version(ctx: &DetectContext<'_>, command: &Command) -> String {
    probed(ctx, command).unwrap_or_else(|| UNKNOWN.to_owned())
}

/// 跑一条命令并取第一行；`--no-version` 时不起进程。
fn probed(ctx: &DetectContext<'_>, command: &Command) -> Option<String> {
    if !ctx.probe_versions {
        return None;
    }
    let text = command.run_text(ctx, ctx.probe_timeout)?;
    let line = first_line(&text);
    (!line.is_empty()).then_some(line)
}

/// 跑一条**配置查询**并取第一行 —— 它是前缀，不是版本，所以 `--no-version` 不影响它。
fn probed_prefix(ctx: &DetectContext<'_>, command: &Command) -> Option<String> {
    let text = command.run_text(ctx, GLOBALS_TIMEOUT)?;
    let line = first_line(&text);
    // `undefined` / `null` 是 npm 在"答不上来"时的 JS 输出，不是路径。
    // 把它当前缀会造出一个不存在的目录，而"拿不到前缀"才是事实。
    if line.is_empty()
        || line.eq_ignore_ascii_case("undefined")
        || line.eq_ignore_ascii_case("null")
    {
        return None;
    }
    Some(line)
}

/// 跑一条枚举命令。
///
/// 返回 `(包, enumerate_error)` —— **两者不会同时有内容**：失败时包一定是空的，
/// 而成功时 slug 一定是 `None`。顺序也是判据的一部分：**超时优先于退出码**
/// （被超时杀掉的进程会带一个非零退出码，把它报成 `command-failed` 就指错了方向）。
///
/// `extra_args` / `env` 只被 tuoen 那一侧用到（[`Command::run_with`]）：机器那一侧
/// 两个都传空表，于是它的行为与 #17 逐字节相同。
fn enumerate(
    ctx: &DetectContext<'_>,
    command: &Command,
    extra_args: &[String],
    env: &[(String, String)],
    parse: fn(&str) -> Result<Vec<GlobalPackage>, &'static str>,
) -> (Vec<GlobalPackage>, Option<String>) {
    let outcome = command.run_with(ctx, extra_args, env, GLOBALS_TIMEOUT);
    if !outcome.spawned {
        return (Vec::new(), Some(COMMAND_FAILED.to_owned()));
    }
    if outcome.timed_out {
        return (Vec::new(), Some(TIMED_OUT.to_owned()));
    }
    if outcome.exit_code != Some(0) {
        return (Vec::new(), Some(COMMAND_FAILED.to_owned()));
    }
    match parse(&outcome.stdout) {
        Ok(packages) => (packages, None),
        Err(slug) => (Vec::new(), Some(slug.to_owned())),
    }
}

/// `npm ls -g --json --depth=0 --offline` 的输出 → 包清单。
///
/// 形状：`{"name":"lib","dependencies":{"<名字>":{"version":"…"}}}`。
///
/// * 顶层不是对象 → [`UNSUPPORTED_OUTPUT`]（我们没跟上 npm 的形状）；
/// * **`dependencies` 键不在** → 空清单：npm 在"一个全局包都没有"时不写这个键，
///   把它判成"形状不认识"会在一台干净的机器上报一个假错误；
/// * `dependencies` 在但不是对象 → [`UNSUPPORTED_OUTPUT`]；
/// * 某一条没有 `version` → `"unknown"`（键永远在，值可以答不上来 —— 与决策 172 同一条规矩）。
fn parse_npm_packages(text: &str) -> Result<Vec<GlobalPackage>, &'static str> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|_| BAD_JSON)?;
    let object = value.as_object().ok_or(UNSUPPORTED_OUTPUT)?;
    let Some(dependencies) = object.get("dependencies") else {
        return Ok(Vec::new());
    };
    let dependencies = dependencies.as_object().ok_or(UNSUPPORTED_OUTPUT)?;

    let mut packages: Vec<GlobalPackage> = dependencies
        .iter()
        .map(|(name, facts)| GlobalPackage {
            name: name.clone(),
            version: facts
                .get("version")
                .and_then(serde_json::Value::as_str)
                .unwrap_or(UNKNOWN)
                .to_owned(),
        })
        .collect();
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

/// `pip list --format=json --disable-pip-version-check` 的输出 → 包清单。
///
/// 形状：`[{"name":"…","version":"…"}]`。顶层不是数组、某一条不是对象、
/// 某一条没有 `name` → [`UNSUPPORTED_OUTPUT`]；没有 `version` → `"unknown"`。
fn parse_pip_packages(text: &str) -> Result<Vec<GlobalPackage>, &'static str> {
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|_| BAD_JSON)?;
    let items = value.as_array().ok_or(UNSUPPORTED_OUTPUT)?;

    let mut packages = Vec::with_capacity(items.len());
    for item in items {
        let object = item.as_object().ok_or(UNSUPPORTED_OUTPUT)?;
        let name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(UNSUPPORTED_OUTPUT)?;
        let version = object
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(UNKNOWN);
        packages.push(GlobalPackage {
            name: name.to_owned(),
            version: version.to_owned(),
        });
    }
    packages.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(packages)
}

/// `pip --version` 里的 python 版本（决策 172）。
///
/// 输出形状：`pip 24.0 from C:\…\Lib\site-packages\pip (python 3.12)`。
/// 取的是**括号里的版本号本身**（`3.12`）—— 括号与 `python` 是 pip 的排版，
/// 不是版本字符串的一部分，而字段名是 `tool_version`，值必须是一个版本
/// （node 那边同理：`node -v` 报的就是 `v24.19.0`，原样存）。
///
/// 形状不认识（没有括号段、括号里不是"数字开头的数字与点"）→ `None` → 走 [`UNKNOWN`]。
fn python_version(answer: &str) -> Option<String> {
    let at = answer.find("(python ")?;
    let rest = &answer[at + "(python ".len()..];
    let version = &rest[..rest.find(')')?];
    if version.is_empty() || !version.starts_with(|c: char| c.is_ascii_digit()) {
        return None;
    }
    if !version.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    Some(version.to_owned())
}

/// `pip --version` 里的安装根目录 —— 决策 174 的 `prefix`。
///
/// 输出形状：`… from C:\Python312\Lib\site-packages\pip (python 3.12)`。
/// 判据 = 从 `from` 后面那条路径里切掉 `\Lib\site-packages…`，**并且要求被切掉的那一段
/// 前面恰好是 `Lib`**：不要求的话，`…\Lib\site-packages` 之外任何含 `site-packages`
/// 的路径（例如某个包自己起的目录名）都会被切出一个假的安装根。
///
/// **形状不认识就不出这个键**（决策 174 的另一半）：宁可没有前缀，
/// 也不要印一个看起来像路径、实际是别的东西的字符串。
fn pip_prefix(answer: &str) -> Option<String> {
    let rest = answer.split_once(" from ")?.1.trim_start();
    let install = rest.split_whitespace().next()?;

    let lower = install.to_ascii_lowercase();
    let at = lower.find("site-packages")?;
    let head = install[..at].trim_end_matches(['\\', '/']);
    let (prefix, lib) = head.rsplit_once(['\\', '/'])?;
    if prefix.is_empty() || !lib.eq_ignore_ascii_case("lib") {
        return None;
    }
    Some(prefix.to_owned())
}

/// 前缀是不是落在"按版本隔离"的目录里（决策 173）。四支**任一为真即为真**：
///
/// ① 前缀本身是 reparse point（junction / symlink）；
/// ② 前缀的**任一祖先**是 reparse point（有界向上走，到盘符根为止）；
/// ③ 前缀路径里有一段像版本号（`v` + 数字，或纯数字点分）；
/// ④ 前缀的 reparse **目标**是这种路径。
///
/// 为什么要这么宽：命中意味着"这个前缀会被版本管理器换掉"，于是快照里的包清单
/// 只对**当下这个版本**成立。本机的 `C:\nvm4w\nodejs` 是 `SymbolicLink` →
/// `…\nvm\v24.19.0`（①④真），切一次 Node 版本，那 7 个全局包就会**静默消失**。
fn inside_version_dir(ctx: &DetectContext<'_>, prefix: &str) -> bool {
    if has_version_segment(prefix) {
        return true; // ③
    }

    let path = Path::new(prefix);
    let facts = ctx.fs.inspect(path);
    if facts.reparse.is_link() {
        return true; // ①
    }
    if let Some(target) = facts.link_target.as_deref()
        && has_version_segment(target)
    {
        return true; // ④
    }

    // ② 祖先。**有界**：盘符根的 `parent()` 是 `None`，所以这个循环自己会停；
    // 另外设一个上限，防的是"某个平台实现给出一个不收敛的 parent 链"。
    let mut current = path.to_path_buf();
    for _ in 0..MAX_ANCESTORS {
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            break;
        };
        if parent == current {
            break;
        }
        if ctx.fs.inspect(&parent).reparse.is_link() {
            return true;
        }
        current = parent;
    }

    false
}

/// 向上找祖先的层数上限。真机上到盘符根远用不到这么多，它是"循环一定会停"的兜底。
const MAX_ANCESTORS: usize = 64;

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::ReparseKind;
    use tuoen_platform::fixture::{FixtureDir, FixturePath, FixtureProcess, MachineFixture};

    use super::*;
    use crate::detect::test_support::DetectFixture;
    // 重定向变量名按**我们自己导出的常量**断言，不写字面量：这一票的判据里，
    // "变量叫什么"是形状的一部分（`root.rs` 顶部那三个 `pub const`）。
    use crate::globals::{NPM_PREFIX_VAR, PIP_USER_VAR, PYTHONUSERBASE_VAR};

    const AT: &str = "2026-10-02T12:00:00Z";

    /// 前缀那一格：一个**普通目录、路径里没有版本段**（四支全假）—— 一真一假里的"假"。
    const NPM_PREFIX: &str = r"C:\tools\npm";
    const PIP_PREFIX: &str = r"C:\Python312";

    const NPM_LS: &str = r#"{
  "name": "lib",
  "dependencies": {
    "zeta": { "version": "2.0.0" },
    "alpha": { "version": "1.0.0" }
  }
}"#;

    const PIP_LIST: &str = r#"[
  { "name": "requests", "version": "2.32.3" },
  { "name": "pip", "version": "24.0" }
]"#;

    const PIP_VERSION_LINE: &str =
        "pip 24.0 from C:\\Python312\\Lib\\site-packages\\pip (python 3.12)\n";

    fn probe(program: &str, args: &[&str], stdout: &str) -> FixtureProcess {
        FixtureProcess {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            stdout: stdout.to_owned(),
            stderr: String::new(),
            exit_code: Some(0),
            timed_out: false,
        }
    }

    /// 一台"两个工具都在 PATH 上、都答得出来"的机器。
    fn machine() -> MachineFixture {
        MachineFixture {
            env: BTreeMap::from([(
                "Path".to_owned(),
                r"C:\tools;C:\Python312\Scripts".to_owned(),
            )]),
            paths: vec![FixturePath::dir(NPM_PREFIX)],
            dirs: vec![
                FixtureDir::new(
                    r"C:\tools",
                    vec![
                        FixturePath::file("npm.cmd", 340),
                        FixturePath::file("node.exe", 84_043_264),
                    ],
                ),
                FixtureDir::new(
                    r"C:\Python312\Scripts",
                    vec![FixturePath::file("pip.exe", 108_544)],
                ),
            ],
            processes: vec![
                probe("node.exe", &["-v"], "v24.19.0\n"),
                probe(
                    "cmd.exe",
                    &["/C", "npm.cmd", "config", "get", "prefix"],
                    "C:\\tools\\npm\n",
                ),
                probe(
                    "cmd.exe",
                    &[
                        "/C",
                        "npm.cmd",
                        "ls",
                        "-g",
                        "--json",
                        "--depth=0",
                        "--offline",
                    ],
                    NPM_LS,
                ),
                probe("pip.exe", &["--version"], PIP_VERSION_LINE),
                probe(
                    "pip.exe",
                    &["list", "--format=json", "--disable-pip-version-check"],
                    PIP_LIST,
                ),
            ],
            registry: Vec::new(),
            managed: Vec::new(),
        }
    }

    /// 改 npm 的 `ls` 那一条（其余不动）。
    fn npm_ls(mutate: impl FnOnce(&mut FixtureProcess)) -> MachineFixture {
        let mut description = machine();
        let entry = description
            .processes
            .iter_mut()
            .find(|process| process.args.last().map(String::as_str) == Some("--offline"))
            .expect("npm ls 那一条");
        mutate(entry);
        description
    }

    /// 改前缀查询那一条的输出。
    fn npm_prefix_answer(answer: &str) -> MachineFixture {
        let mut description = machine();
        let entry = description
            .processes
            .iter_mut()
            .find(|process| process.args.contains(&"config".to_owned()))
            .expect("prefix 那一条");
        entry.stdout = answer.to_owned();
        description
    }

    fn collect(description: &MachineFixture) -> GlobalsFile {
        collect_globals(&DetectFixture::build(description).context(), AT)
    }

    fn row<'a>(file: &'a GlobalsFile, tool: &str) -> &'a GlobalRow {
        file.global
            .iter()
            .find(|row| row.tool == tool)
            .unwrap_or_else(|| panic!("没有 `{tool}` 行：{:?}", file.global))
    }

    fn names(row: &GlobalRow) -> Vec<&str> {
        row.packages
            .iter()
            .map(|package| package.name.as_str())
            .collect()
    }

    #[test]
    fn every_tool_on_path_gets_a_row_with_its_version_prefix_and_sorted_packages() {
        let file = collect(&machine());
        assert_eq!(file.global.len(), 2, "{:?}", file.global);

        let npm = row(&file, "npm");
        assert_eq!(npm.tool_version, "v24.19.0", "原样来自 `node -v`");
        assert_eq!(npm.prefix.as_deref(), Some(NPM_PREFIX));
        assert_eq!(npm.prefix_inside_version_dir, Some(false));
        assert_eq!(names(npm), ["alpha", "zeta"], "按 name 排序");
        assert_eq!(npm.packages[0].version, "1.0.0");
        assert_eq!(npm.enumerate_error, None);

        let pip = row(&file, "pip");
        assert_eq!(pip.tool_version, "3.12", "`(python 3.12)` 里的版本号本身");
        assert_eq!(pip.prefix.as_deref(), Some(PIP_PREFIX));
        assert_eq!(pip.prefix_inside_version_dir, Some(false));
        assert_eq!(names(pip), ["pip", "requests"]);
    }

    /// 决策 171：**找不到可执行文件的工具不产生行**（不是"报了个空行"）。
    #[test]
    fn a_tool_whose_executable_is_not_on_path_produces_no_row() {
        let mut description = machine();
        description
            .env
            .insert("Path".to_owned(), r"C:\Windows".to_owned());
        let fixture = DetectFixture::build(&description);
        let file = collect_globals(&fixture.context(), AT);
        assert!(file.global.is_empty(), "{:?}", file.global);
        // 而且**一次进程都没起**：找不到就不问。
        assert!(fixture.machine.runner.calls().is_empty());
    }

    /// 决策 172：`tool_version` 永远出键，答不上来写 `unknown`。
    #[test]
    fn an_unanswerable_version_is_unknown_and_the_key_stays() {
        let mut description = machine();
        description
            .processes
            .retain(|process| process.program != "node.exe");
        let file = collect(&description);
        let npm = row(&file, "npm");
        assert_eq!(npm.tool_version, UNKNOWN);
        // 版本没了，其余照旧 —— "答不上版本"不许把整行带走。
        assert_eq!(npm.prefix.as_deref(), Some(NPM_PREFIX));
        assert_eq!(names(npm), ["alpha", "zeta"]);
    }

    /// `--no-version`（`probe_versions == false`）：**不起版本探测的进程**，
    /// 但枚举与配置查询照旧（决策 172 的键永远在 ⇒ 值退化成 `unknown`）。
    #[test]
    fn no_version_skips_the_probes_and_keeps_the_enumeration() {
        let fixture = DetectFixture::build(&machine()).without_probing();
        let file = collect_globals(&fixture.context(), AT);

        assert_eq!(row(&file, "npm").tool_version, UNKNOWN);
        assert_eq!(row(&file, "pip").tool_version, UNKNOWN);
        assert_eq!(row(&file, "npm").prefix.as_deref(), Some(NPM_PREFIX));
        // pip 的 prefix 与版本**同一条命令**（决策 174 的同生共死）：
        // 不探测就没有前缀，两个键一起不出 —— 而不是印一个假的 `false`。
        assert_eq!(row(&file, "pip").prefix, None);
        assert_eq!(row(&file, "pip").prefix_inside_version_dir, None);
        assert_eq!(names(row(&file, "npm")), ["alpha", "zeta"]);

        let calls = fixture.machine.runner.calls();
        let programs: Vec<&str> = calls.iter().map(|call| call.program.as_str()).collect();
        assert!(
            !programs.contains(&"node.exe"),
            "`node -v` 是版本探测，一条都不许跑：{programs:?}"
        );
        // pip 那两条：`--version`（探测）不许跑，`list`（枚举）照旧。
        let pip_version_ran = calls.iter().any(|call| {
            call.program == "pip.exe" && call.args.first().map(String::as_str) == Some("--version")
        });
        assert!(!pip_version_ran, "`pip --version` 是版本探测：{programs:?}");
        assert!(
            programs.contains(&"cmd.exe") && programs.contains(&"pip.exe"),
            "枚举与前缀查询照旧：{programs:?}"
        );
    }

    /// 决策 175：四种失败形态是四个 slug，**行本身照样写出去**。
    #[test]
    fn the_four_enumeration_failures_are_four_distinct_slugs() {
        let cases: [(&str, MachineFixture); 4] = [
            (
                "command-failed",
                npm_ls(|process| {
                    process.stdout = String::new();
                    process.stderr = "npm error: cannot reach the registry\n".to_owned();
                    process.exit_code = Some(1);
                }),
            ),
            (
                "timed-out",
                npm_ls(|process| {
                    process.stdout = String::new();
                    process.exit_code = None;
                    process.timed_out = true;
                }),
            ),
            (
                "bad-json",
                npm_ls(|process| {
                    process.stdout =
                        "npm WARN config production Use `--omit=dev` instead.\n".to_owned();
                }),
            ),
            (
                "unsupported-output",
                // 能解析、但形状不认识（npm 从不这么答，这正是"我们没跟上"的形状）。
                npm_ls(|process| process.stdout = "[\"alpha\"]\n".to_owned()),
            ),
        ];

        for (slug, description) in cases {
            let file = collect(&description);
            let npm = row(&file, "npm");
            assert_eq!(npm.enumerate_error.as_deref(), Some(slug), "{slug}");
            assert!(npm.packages.is_empty(), "{slug}：数不出包");
            // 行本身带着 prefix 与 tool_version —— "发现了前缀，但数不出包"。
            assert_eq!(npm.tool_version, "v24.19.0", "{slug}");
            assert_eq!(npm.prefix.as_deref(), Some(NPM_PREFIX), "{slug}");
        }
    }

    /// 决策 175：**超时优先于退出码**。被超时杀掉的进程会带一个非零退出码，
    /// 把它报成 `command-failed` 就指错了方向（修法完全不同）。
    #[test]
    fn a_timeout_with_a_nonzero_exit_code_is_still_a_timeout() {
        let description = npm_ls(|process| {
            process.exit_code = Some(1);
            process.timed_out = true;
        });
        let file = collect(&description);
        assert_eq!(
            row(&file, "npm").enumerate_error.as_deref(),
            Some("timed-out")
        );
    }

    /// 决策 174：`prefix` 与 `prefix_inside_version_dir` **同生共死**。
    #[test]
    fn prefix_and_the_version_dir_flag_live_and_die_together() {
        // 前缀那条命令答不上来（`undefined` 是 npm 的"答不上来"）。
        for answer in ["undefined\n", "\n", "null\n"] {
            let file = collect(&npm_prefix_answer(answer));
            let npm = row(&file, "npm");
            assert_eq!(npm.prefix, None, "{answer:?}");
            assert_eq!(npm.prefix_inside_version_dir, None, "{answer:?}");
            // 前缀没了，包照旧数出来 —— 两件事互不依赖。
            assert_eq!(names(npm), ["alpha", "zeta"], "{answer:?}");
        }

        // 命令根本没跑起来（把 cmd.exe 从固定装置里撤掉）。
        let mut description = machine();
        description
            .processes
            .retain(|process| process.program != "cmd.exe");
        let file = collect(&description);
        assert_eq!(row(&file, "npm").prefix, None);
        assert_eq!(row(&file, "npm").prefix_inside_version_dir, None);
    }

    /// 决策 173：四支判据**各自**都能把结论判成真（一条只判其中一支的实现会红）。
    #[test]
    fn each_of_the_four_branches_alone_flags_the_prefix() {
        // ③ 路径里有一段像版本号（没有任何 reparse）。
        let file = collect(&npm_prefix_answer("C:\\tools\\v24\\npm\n"));
        assert_eq!(row(&file, "npm").prefix_inside_version_dir, Some(true), "③");

        // ① 前缀本身是 reparse point（目标里**没有**版本段 ⇒ ④ 不成立）。
        let mut description = npm_prefix_answer("C:\\tools\\npm\n");
        description.paths = vec![FixturePath::symlink_dir(
            NPM_PREFIX,
            r"C:\Users\x\AppData\Local\nvm\current",
        )];
        assert_eq!(
            row(&collect(&description), "npm").prefix_inside_version_dir,
            Some(true),
            "①"
        );

        // ④ reparse 目标是版本目录。用 `Other`（**不是** junction/symlink ⇒ ① 不成立）
        // 把 ④ 单独隔离出来：`is_link()` 对 `Other` 是假，而目标里 `v24.19.0` 是真。
        let mut description = npm_prefix_answer("C:\\tools\\npm\n");
        let mut entry = FixturePath::dir(NPM_PREFIX);
        entry.reparse = ReparseKind::Other(0x9000_0001);
        entry.link_target = Some(r"C:\Users\x\AppData\Local\nvm\v24.19.0".to_owned());
        description.paths = vec![entry];
        assert_eq!(
            row(&collect(&description), "npm").prefix_inside_version_dir,
            Some(true),
            "④"
        );

        // ② 前缀的**祖先**是 reparse point（前缀自己不是、路径里也没有版本段）。
        let mut description = npm_prefix_answer("C:\\tools\\npm\n");
        description.paths = vec![FixturePath::symlink_dir(r"C:\tools", r"C:\elsewhere\tools")];
        assert_eq!(
            row(&collect(&description), "npm").prefix_inside_version_dir,
            Some(true),
            "②"
        );

        // 四支全假 —— 一真一假里的"假"，否则"恒 true"的实现也能过。
        assert_eq!(
            row(&collect(&machine()), "npm").prefix_inside_version_dir,
            Some(false),
            "四支全假"
        );
    }

    /// 决策 173 的真机形状：`C:\nvm4w\nodejs` 是符号链接、目标是 `…\nvm\v24.19.0`。
    #[test]
    fn the_real_machines_npm_prefix_is_inside_a_version_directory() {
        let mut description = machine();
        description.paths = vec![FixturePath::symlink_dir(
            NPM_PREFIX,
            r"C:\Users\x\AppData\Local\nvm\v24.19.0",
        )];
        let file = collect(&description);
        assert_eq!(row(&file, "npm").prefix_inside_version_dir, Some(true));
        // pip 那边是普通目录、路径里没有版本段 ⇒ 同一次采集里一真一假。
        assert_eq!(row(&file, "pip").prefix_inside_version_dir, Some(false));
    }

    /// pip 的 `prefix` 来自安装路径的形状；形状不认识就**不出这个键**。
    #[test]
    fn the_pip_prefix_comes_from_the_install_path_shape() {
        assert_eq!(pip_prefix(PIP_VERSION_LINE).as_deref(), Some(PIP_PREFIX));
        // venv 里装的那一份：前缀是 venv 根，不是 python 安装根。
        assert_eq!(
            pip_prefix("pip 24.0 from C:\\venv\\Lib\\site-packages\\pip (python 3.12)").as_deref(),
            Some(r"C:\venv")
        );
        // 形状不认识：没有 ` from `、没有 `site-packages`、`Lib` 那一段不在。
        assert_eq!(pip_prefix("pip 24.0\n"), None);
        assert_eq!(
            pip_prefix("pip 24.0 from C:\\Python312\\pip (python 3.12)"),
            None
        );
        assert_eq!(
            pip_prefix("pip 24.0 from C:\\Python312\\Lib\\other\\pip (python 3.12)"),
            None
        );
    }

    /// 决策 172：pip 的 `tool_version` 是**括号里的版本号本身**。
    #[test]
    fn the_pip_version_is_the_bare_number_inside_the_parentheses() {
        assert_eq!(python_version(PIP_VERSION_LINE).as_deref(), Some("3.12"));
        assert_eq!(
            python_version("pip 24.0 from C:\\x (python 3.13.1)\n").as_deref(),
            Some("3.13.1")
        );
        // 形状不认识 → `None` → 走 `unknown`（不是把整行塞进版本字段）。
        assert_eq!(python_version("pip 24.0\n"), None);
        assert_eq!(python_version("pip 24.0 (python 3.x)\n"), None);
        assert_eq!(python_version("pip 24.0 (python )\n"), None);
    }

    // ---------------- 两个来源（ticket #23） ----------------

    /// 我们自己的根（`%LOCALAPPDATA%\tuoen\globals`）。
    const LOCAL: &str = r"C:\Users\dev\AppData\Local";
    const BASE: &str = r"C:\Users\dev\AppData\Local\tuoen\globals";
    const NPM_ROOT: &str = r"C:\Users\dev\AppData\Local\tuoen\globals\npm\v24.19.0";
    const PIP_ROOT: &str = r"C:\Users\dev\AppData\Local\tuoen\globals\pip";

    /// 机器自己的 7 个全局包 —— **真机 #17 的逐字数字**。
    const NPM_LS_REAL: &str = r#"{
  "name": "lib",
  "dependencies": {
    "@deepseek-ai/dsh": { "version": "0.1.0-rc.6" },
    "@openai/codex": { "version": "0.160.0" },
    "billion-context": { "version": "0.1.179" },
    "corepack": { "version": "0.35.0" },
    "npm": { "version": "11.17.0" },
    "pnpm": { "version": "11.21.0" },
    "tokentracker-cli": { "version": "0.87.3" }
  }
}"#;

    /// 我们的根里那 3 个。**名字在机器那一份里也有**（这是故意的）：
    /// 于是一个"按包名合并两个来源"的实现会静默通过 —— 而它丢掉的正是
    /// "这个包该装进哪个根"。
    const NPM_LS_TUOEN: &str = r#"{
  "name": "lib",
  "dependencies": {
    "@deepseek-ai/dsh": { "version": "0.1.0-rc.6" },
    "corepack": { "version": "0.35.0" },
    "pnpm": { "version": "11.21.0" }
  }
}"#;

    /// pip 的机器那一份 —— 真机 #17 的 3 个。
    const PIP_LIST_REAL: &str = r#"[
  { "name": "pip", "version": "25.0.1" },
  { "name": "pypdf", "version": "6.19.0" },
  { "name": "pypinyin", "version": "0.55.0" }
]"#;

    /// pip 的重定向那一条：正好只列 tuoen 管的那些（实测）。
    const PIP_LIST_TUOEN: &str = r#"[
  { "name": "pip", "version": "25.0.1" },
  { "name": "pypinyin", "version": "0.55.0" }
]"#;

    /// 省略一个键的辅助：`FixtureProcess` 的字面量太长，测试里只关心 stdout 的变体多。
    fn with_stdout(mut process: FixtureProcess, stdout: &str) -> FixtureProcess {
        process.stdout = stdout.to_owned();
        process
    }

    /// 一台"两个工具都在、机器自己 npm 7 个 + pip 3 个、我们的根里 npm 3 个"的机器。
    ///
    /// pip 的 `--user` 那一问答**空表**：这个固定装置要的形状是"根存在、里面没有包"。
    fn two_source_machine() -> MachineFixture {
        let mut description = machine();
        description
            .env
            .insert("LOCALAPPDATA".to_owned(), LOCAL.to_owned());
        description.dirs.extend([
            FixtureDir::new(NPM_ROOT, vec![FixturePath::dir("node_modules")]),
            // 假的文件系统**不合成父目录**：`<base>\pip` 与它下面那一层都要显式声明，
            // 否则 `inspect(<base>\pip).exists` 是假 —— 于是 `roots` 说它不存在，
            // 而这一份固定装置想表达的是"根在、里面是空的"。
            FixtureDir::new(PIP_ROOT, vec![FixturePath::dir("Python312")]),
            FixtureDir::new(
                &format!(r"{PIP_ROOT}\Python312\Scripts"),
                vec![FixturePath::file("pip.exe", 108_544)],
            ),
        ]);
        description.processes.extend([
            probe(
                "cmd.exe",
                &[
                    "/C",
                    "npm.cmd",
                    "ls",
                    "-g",
                    "--json",
                    "--depth=0",
                    "--offline",
                    "--prefix",
                    NPM_ROOT,
                ],
                NPM_LS_TUOEN,
            ),
            probe(
                "pip.exe",
                &[
                    "list",
                    "--user",
                    "--format=json",
                    "--disable-pip-version-check",
                ],
                "[]",
            ),
        ]);
        // 机器那一份换成真机的 7 个 + 3 个（原来那两个是为了别处的小形状）。
        let entry = description
            .processes
            .iter_mut()
            .find(|process| process.args.last().map(String::as_str) == Some("--offline"))
            .expect("npm ls 那一条");
        *entry = with_stdout(entry.clone(), NPM_LS_REAL);
        let entry = description
            .processes
            .iter_mut()
            .find(|process| process.args.contains(&"--format=json".to_owned()))
            .expect("pip list 那一条");
        *entry = with_stdout(entry.clone(), PIP_LIST_REAL);
        description
    }

    /// 在 [`two_source_machine`] 之上把 pip 的 `--user` 那一问答成两个包。
    fn pip_tuoen_machine() -> MachineFixture {
        let mut description = two_source_machine();
        let entry = description
            .processes
            .iter_mut()
            .find(|process| process.args.contains(&"--user".to_owned()))
            .expect("pip list --user 那一条");
        *entry = with_stdout(entry.clone(), PIP_LIST_TUOEN);
        description
    }

    /// 按（工具，来源）找一行 —— `source` 是 ticket #23 之后**必须**一起判的那个维度。
    fn row_of<'a>(file: &'a GlobalsFile, tool: &str, source: &str) -> &'a GlobalRow {
        file.global
            .iter()
            .find(|row| row.tool == tool && row.source == source)
            .unwrap_or_else(|| panic!("没有 `{tool}`/`{source}` 行：{:?}", file.global))
    }

    /// 某个来源的**全部包**（跨工具），按（工具，包名）—— 用来数"只问这一个来源
    /// 会漏掉几个"。
    fn packages_of(file: &GlobalsFile, source: &str) -> Vec<(String, String)> {
        let mut packages: Vec<(String, String)> = file
            .global
            .iter()
            .filter(|row| row.source == source)
            .flat_map(|row| {
                row.packages
                    .iter()
                    .map(|package| (row.tool.clone(), package.name.clone()))
            })
            .collect();
        packages.sort();
        packages
    }

    /// 同一个工具的两个来源是**两行**（决策 166 的加法变更），而且行序固定。
    #[test]
    fn the_two_sources_are_two_rows_in_a_fixed_order() {
        let file = collect(&two_source_machine());
        let rows: Vec<(&str, &str)> = file
            .global
            .iter()
            .map(|row| (row.tool.as_str(), row.source.as_str()))
            .collect();
        assert_eq!(
            rows,
            [("npm", "machine"), ("npm", "tuoen"), ("pip", "machine")],
            "机器自己的在前；pip 的根里没有包 ⇒ 没有那一行：{:?}",
            file.global
        );

        let machine = row_of(&file, "npm", "machine");
        assert_eq!(names(machine).len(), 7, "真机 #17 的 7 个");
        let tuoen = row_of(&file, "npm", "tuoen");
        assert_eq!(names(tuoen), ["@deepseek-ai/dsh", "corepack", "pnpm"]);
        assert_eq!(tuoen.prefix.as_deref(), Some(NPM_ROOT));
        assert_eq!(tuoen.tool_version, "v24.19.0", "同一份运行时版本");
        assert_eq!(
            tuoen.prefix_inside_version_dir,
            Some(true),
            "按版本隔离是**设计**"
        );
        assert_eq!(machine.prefix_inside_version_dir, Some(false));
    }

    /// **反例**：固定装置"机器 7 个 + tuoen 3 个" → 只问机器来源会**漏掉那 3 个**。
    ///
    /// 而且这三个**包名在机器那一份里也有** —— 所以"按名字合并两个来源"补不回来：
    /// 漏掉的是三行**包副本**（`source` 不同），而 `restore` 正是按 `source` 决定
    /// 往哪个根里装的。这个固定装置把"去重合并"这条捷径钉死在红里。
    #[test]
    fn asking_the_machine_source_alone_misses_the_three_tuoen_packages() {
        let file = collect(&two_source_machine());
        let machine = packages_of(&file, "machine");
        let tuoen = packages_of(&file, "tuoen");

        assert_eq!(machine.len(), 10, "机器：npm 7 + pip 3（真机 #17）");
        assert_eq!(tuoen.len(), 3, "tuoen：npm 3");
        assert_eq!(
            machine.len() + tuoen.len(),
            13,
            "两个来源都要在，一个都不许被合并掉"
        );

        // 只问机器来源的那一份清单里，`source = "tuoen"` 这三行**一行都没有**。
        assert!(
            file.global
                .iter()
                .filter(|row| row.source == "machine")
                .all(|row| row.tool != "npm" || row.packages.len() != 3),
            "机器那一行不该长出 tuoen 的包"
        );
        for (tool, name) in &tuoen {
            assert!(
                machine.contains(&(tool.clone(), name.clone())),
                "这个固定装置故意让名字重叠：{tool}/{name}"
            );
        }
        // 名字重叠 ⇒ 按名字去重会从 13 变 10，静默丢掉三个"该装到哪儿"的答案。
        let mut by_name: Vec<(String, String)> = machine.clone();
        by_name.extend(tuoen.clone());
        by_name.sort();
        by_name.dedup();
        assert_eq!(by_name.len(), 10, "按名字去重就只剩 10 个");
        assert_ne!(by_name.len(), machine.len() + tuoen.len());
    }

    /// tuoen 的 npm 那一问：`--prefix <根>` **与** `NPM_CONFIG_PREFIX` 是同一个值。
    ///
    /// 两条路都走是设计的一部分（一条写错另一条还在），但它们指向**不同**目录时，
    /// 读到的树与 npm 自己认为的前缀就不是一回事 —— 那会造出一份假清单。
    #[test]
    fn the_tuoen_npm_question_carries_the_root_in_the_argv_and_in_the_environment() {
        let fixture = DetectFixture::build(&two_source_machine());
        let _ = collect_globals(&fixture.context(), AT);

        let call = fixture
            .machine
            .runner
            .calls()
            .into_iter()
            .find(|call| call.args.last().map(String::as_str) == Some(NPM_ROOT))
            .expect("带 `--prefix <根>` 的那一问");
        assert_eq!(call.program, "cmd.exe");
        assert_eq!(
            call.args,
            [
                "/C",
                "npm.cmd",
                "ls",
                "-g",
                "--json",
                "--depth=0",
                "--offline",
                "--prefix",
                NPM_ROOT
            ]
        );
        assert_eq!(
            call.env,
            [(NPM_PREFIX_VAR.to_owned(), NPM_ROOT.to_owned())],
            "子进程环境里的前缀必须与 `--prefix` 同值"
        );
    }

    /// tuoen 的 pip 那一问：`PYTHONUSERBASE` + `PIP_USER=1`，并且要 `--user`。
    #[test]
    fn the_tuoen_pip_question_redirects_the_user_base_and_asks_for_user_only() {
        let fixture = DetectFixture::build(&pip_tuoen_machine());
        let _ = collect_globals(&fixture.context(), AT);

        let call = fixture
            .machine
            .runner
            .calls()
            .into_iter()
            .find(|call| call.args.iter().any(|arg| arg == "--user"))
            .expect("`pip list --user`");
        assert_eq!(call.program, "pip.exe", "**绝不是** `python`");
        assert_eq!(
            call.args,
            [
                "list",
                "--user",
                "--format=json",
                "--disable-pip-version-check"
            ]
        );
        assert_eq!(
            call.env,
            [
                (PYTHONUSERBASE_VAR.to_owned(), PIP_ROOT.to_owned()),
                (PIP_USER_VAR.to_owned(), "1".to_owned()),
            ]
        );

        let file = collect(&pip_tuoen_machine());
        let tuoen = row_of(&file, "pip", "tuoen");
        assert_eq!(names(tuoen), ["pip", "pypinyin"], "重定向之后只列我们的");
        assert_eq!(
            tuoen.prefix.as_deref(),
            Some(PIP_ROOT),
            "`PYTHONUSERBASE` 就是那一行的前缀（`Python312\\` 是 pip 自己插的）"
        );
        assert_eq!(tuoen.tool_version, "3.12");
        // 机器那一份是**另外一个**答案，两条都在（互不覆盖）。
        assert_eq!(
            names(row_of(&file, "pip", "machine")),
            ["pip", "pypdf", "pypinyin"]
        );
    }

    /// 根存在但里面一个包都没有：**不产生行**（不是产生一行空的）。
    #[test]
    fn an_empty_root_produces_no_row_but_the_question_is_still_asked() {
        let file = collect(&two_source_machine());
        assert!(
            file.global
                .iter()
                .all(|row| !(row.tool == "pip" && row.source == "tuoen")),
            "空的根不该长出一行：{:?}",
            file.global
        );

        // 但它是**问过**的（根存在 ⇒ 值得问），而且"根在哪里、它是空的"说得出来。
        let fixture = DetectFixture::build(&two_source_machine());
        let ctx = fixture.context();
        let root = root_from_context(&ctx).expect("有 LOCALAPPDATA");
        let collected = collect_rows(&ctx, Some(&root));
        assert_eq!(collected.roots.len(), 2, "两个根都在");
        assert!(
            collected.roots.iter().all(|row| row.exists),
            "两个目录都在磁盘上：{:?}",
            collected.roots
        );
        assert!(
            collected.notes.is_empty(),
            "什么都没缺，就没有 notes：{:?}",
            collected.notes
        );
    }

    /// 根**不存在**：一次进程都不起（铁律 3），也不产生 tuoen 行。
    #[test]
    fn a_root_that_does_not_exist_costs_no_process_at_all() {
        let mut description = two_source_machine();
        description.dirs.retain(|dir| !dir.path.starts_with(BASE));
        description.processes.retain(|process| {
            !process.args.contains(&"--prefix".to_owned())
                && !process.args.contains(&"--user".to_owned())
        });
        let fixture = DetectFixture::build(&description);
        let file = collect_globals(&fixture.context(), AT);

        assert_eq!(file.global.len(), 2, "只有机器那两行：{:?}", file.global);
        assert!(file.global.iter().all(|row| row.source == "machine"));

        let asked = fixture
            .machine
            .runner
            .calls()
            .into_iter()
            .filter(|call| {
                call.args
                    .iter()
                    .any(|arg| arg == "--prefix" || arg == "--user")
            })
            .collect::<Vec<_>>();
        assert!(
            asked.is_empty(),
            "根不存在就不该问（那要启动包管理器）：{asked:?}"
        );

        // 而"根在哪里、它是空的"照旧说得出来。
        let ctx = fixture.context();
        let root = root_from_context(&ctx).expect("有 LOCALAPPDATA");
        let collected = collect_rows(&ctx, Some(&root));
        assert_eq!(collected.roots.len(), 2);
        assert!(collected.roots.iter().all(|row| !row.exists));
    }

    /// 根存在、但那一问**失败**：行照样写出去，带 `enumerate_error`（决策 175）。
    ///
    /// 这一条防的是"tuoen 那一半失败时静默消失" —— 那会让 `restore` 以为
    /// "我们没管过任何包"，而事实是"我们没数出来"。
    #[test]
    fn a_failing_tuoen_question_still_writes_the_row_with_the_error_slug() {
        let mut description = two_source_machine();
        // 让"带 `--prefix <根>`"那一问非零退出 —— 注意**不能**只是把那条固定装置
        // 撤掉：决策 185 的匹配是"声明的前缀命中"，于是机器那一问（更短的那条）
        // 会接着回答它，这一问看起来就"成功"了（那正是这条用例的第一个版本踩的坑）。
        let entry = description
            .processes
            .iter_mut()
            .find(|process| process.args.last().map(String::as_str) == Some(NPM_ROOT))
            .expect("带 `--prefix <根>` 的那一问");
        entry.stdout = String::new();
        entry.stderr = "npm error: EACCES\n".to_owned();
        entry.exit_code = Some(1);

        let file = collect(&description);
        let tuoen = row_of(&file, "npm", "tuoen");
        assert_eq!(tuoen.enumerate_error.as_deref(), Some("command-failed"));
        assert!(tuoen.packages.is_empty());
        assert_eq!(tuoen.prefix.as_deref(), Some(NPM_ROOT), "行本身照样带前缀");
    }

    /// 版本判不出来 ⇒ **没有 npm 的根**（决策 26 的按版本隔离），原因是一条 note。
    #[test]
    fn an_unknown_runtime_version_means_no_npm_root_and_a_note() {
        let mut description = two_source_machine();
        description
            .processes
            .retain(|process| process.program != "node.exe");
        let fixture = DetectFixture::build(&description);
        let ctx = fixture.context();
        let root = root_from_context(&ctx).expect("有 LOCALAPPDATA");
        let collected = collect_rows(&ctx, Some(&root));

        assert!(
            collected
                .rows
                .iter()
                .all(|row| row.source == GlobalsSource::Machine || row.tool == GlobalsTool::Pip),
            "npm 的 tuoen 行不该出现：{:?}",
            collected.rows
        );
        let roots: Vec<GlobalsTool> = collected.roots.iter().map(|row| row.tool).collect();
        assert_eq!(roots, [GlobalsTool::Pip], "只有 pip 的根算得出来");
        assert_eq!(
            collected.notes,
            [GlobalsNote {
                tool: GlobalsTool::Npm,
                code: GlobalsNoteCode::RuntimeVersionUnknown,
            }]
        );
    }

    /// 进程环境里没有 `LOCALAPPDATA`（决策 187）：**`capture` 照旧采机器那一份**。
    ///
    /// （`tuoen globals list` 在同一个输入下是**错误** —— 那条命令的一半答案就是根。
    /// 两条路的分歧是刻意的：快照少一半仍然是快照，而"根在哪"答不出来就是答不出来。）
    #[test]
    fn without_local_app_data_the_capture_keeps_the_machine_rows() {
        let mut description = two_source_machine();
        description.env.remove("LOCALAPPDATA");
        let file = collect(&description);
        assert_eq!(file.global.len(), 2, "{:?}", file.global);
        assert!(file.global.iter().all(|row| row.source == "machine"));
    }

    /// `globals.toml` 的 `source` **永远出键**（决策 166：加一个键是加法变更）。
    #[test]
    fn the_source_key_is_always_written_to_globals_toml() {
        let file = collect(&two_source_machine());
        let text = toml::to_string(&file).expect("可序列化");

        let sources: Vec<&str> = text
            .lines()
            .filter_map(|line| line.trim().strip_prefix("source = "))
            .collect();
        assert_eq!(
            sources,
            ["\"machine\"", "\"tuoen\"", "\"machine\""],
            "每一行都有 source：\n{text}"
        );
        // 快照里 `source` 是**普通键**（不是 `Option`）⇒ 没有"这一行不知道来源"的形态。
        assert_eq!(
            text.matches("source = ").count(),
            file.global.len(),
            "{text}"
        );
    }

    /// 同一个假机器跑两次，除 `captured_at` 外逐字节相同（确定性是幂等性的一部分）。
    #[test]
    fn two_runs_of_the_same_machine_are_identical() {
        let description = machine();
        assert_eq!(collect(&description), collect(&description));
    }
}
