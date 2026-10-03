//! `tuoen capture` 的执行、`--json` 形状与人类输出。
//!
//! # 这一层做什么
//!
//! 装配**六个真实后端**（照 `main.rs` 的 `run_detect` 的样子 —— 检测引擎是唯一一处
//! "读这台机器"的代码，在这里另写一套会让两份事实慢慢漂移），把 `--only` / `--out` /
//! `--no-version` 翻译成 [`CaptureOptions`]，调 [`capture`] 拿 bundle、调
//! [`write_bundle`] 落盘，然后把 bundle 里的数字印成中文，或者序列化成稳定 JSON。
//!
//! **这一层不读机器、不读文件、不算判据。** 报告里的每一个数字都从
//! [`CaptureBundle`] 里算出来，于是"报告"与"刚写出去的文件"不可能互相矛盾 ——
//! 重新读一遍刚写的文件会得到第三个真相：磁盘上的那一份。
//!
//! # 为什么视图与打印在同一个文件里
//!
//! 仓库的习惯是每个命令族一个 `*_view.rs`（`path_view` / `shim_view` / `manage_view`）。
//! 这一族只有一条命令、一个消费者，而这一票允许改动的文件是固定的 ——
//! 所以视图与打印合在这里，模块文档负责说明它们的契约。若将来 `capture` 长出
//! 第二个消费者（`restore` 回读、GUI），这里就该拆出 `capture_view.rs`。

use std::collections::BTreeMap;

use serde::Serialize;
use tuoen_core::capture::{
    CaptureBundle, CaptureError, CaptureOptions, ConfigsFile, EnvFile, Existence, GlobalsFile,
    PathFile, SkipEntry, ToolsFile, WslFile, capture, write_bundle,
};
use tuoen_platform::EnvScope;

use crate::capture::CaptureArgs;
use crate::envelope::Envelope;
use crate::exit;

// ─────────────────────────────────────────────────────────────────────────────
// 编排
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen capture`。返回退出码。
pub fn run(args: &CaptureArgs) -> i32 {
    // 六个真实依赖来自**唯一**一处装配（`detect_ctx::Backends`）—— 捕获**不是**另一套
    // 检测逻辑，它是"把检测结果与其余三样东西写成文件"。两套装配迟早会读出两台机器。
    let backends = crate::detect_ctx::Backends::assemble();
    // `--no-version` 只影响**探测**：条目照旧被发现，只是没有版本号。
    let ctx = backends.context(!args.no_version);

    let mut opts = CaptureOptions::all(args.out.clone(), &tuoen_store::now_rfc3339());
    // **空向量就是"全部"**（`CaptureOptions` 的既定语义），所以这里直接转发：
    // 一个忘了传参的调用不该安静地什么都不做。
    opts.sections = args.only.iter().map(|section| section.section()).collect();
    // 我们自己的两个根 —— 它是 `owner = "tuoen"` 的**唯一**判据（决定还原时哪些
    // `PATH` 条目可以动）。两个根来自**同一个** `Store` 实例（`Backends` 保证这件事），
    // 而且 shim 目录用 `shim_cmd::shim_dir`（全仓唯一一份定义），不自己拼路径：
    // 第二份定义会漂移，而它漂移的后果是快照里一整类条目的归属出错。
    opts.tuoen_roots = vec![
        backends.store_root().to_path_buf(),
        backends.shim_dir().to_path_buf(),
    ];

    let bundle = match capture(&ctx, &opts) {
        Ok(bundle) => bundle,
        Err(error) => return report_failure(args, &opts, &error),
    };
    let written = match write_bundle(&bundle, &opts.out_dir) {
        Ok(written) => written,
        Err(error) => return report_failure(args, &opts, &error),
    };
    // 落盘条数与"这次会写哪些文件"必须一致：报告里的文件清单是按后者印的，
    // 而不一致意味着有人改了 `write_bundle` 与 `file_names` 其中之一。
    debug_assert_eq!(written.len(), bundle.file_names().len());

    let view = CaptureView::new(&bundle, &opts);
    if args.json {
        crate::print_json(&Envelope::ok("capture", &view));
    } else {
        print_human(&view, args.no_version);
    }
    exit::SUCCESS
}

/// 失败路径。
///
/// # 为什么是 `partial` 而不是 `err`
///
/// 失败发生在**落盘**这一步，而捕获本身已经做完了（bundle 在内存里）——
/// "写到哪、这一次捕获了哪几个 section"是**已经发生的事实**。用纯 `err` 把它们
/// 丢掉，消费者就只剩一句"失败了"，连往哪个目录重试都不知道。
///
/// # 为什么失败载荷里没有 `files` 键
///
/// `write_bundle` 是**逐个文件**写的：第三个文件失败时，前两个已经在磁盘上了。
/// 报一个空数组是在说假话（"什么都没写"），而准确清单它没给 —— 所以那个键
/// **缺席**。"缺席 = 我们不知道"比"空数组 = 我们知道是零"诚实，这个区别
/// 与 `sections` 上"没捕获"vs"没有"是同一个。
fn report_failure(args: &CaptureArgs, opts: &CaptureOptions, error: &CaptureError) -> i32 {
    if args.json {
        crate::print_json(&Envelope::partial(
            "capture",
            error.code(),
            error.to_string(),
            &CaptureAttemptView::new(opts),
        ));
    } else {
        eprintln!("tuoen: {error}");
        eprintln!(
            "（没有写出完整的快照。目标目录：{}）",
            opts.out_dir.display()
        );
    }
    exit::RUNTIME_ERROR
}

// ─────────────────────────────────────────────────────────────────────────────
// `--json` 形状
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen capture --json` 的成功载荷。
///
/// # 两条硬规矩
///
/// 1. **我们写的字符串里不出现中文**（决策 35）。所以结论性的东西一律是**稳定 slug**
///    （section 名、置信度、`PATH` 档位、作用域），而**中文只进人类输出**。
///    这条规矩有一个看得见的代价：[`SkippedView::reason`] 天生是一句中文，
///    于是它不进 JSON —— 脚本要的判据是 `kind` 那个 slug，人看的是人类输出。
/// 2. **每个 `Option` 的含义都是「这一次没捕获」**，不是"这台机器上没有"。
///    `tools: null` 与 `tolols` 的空数组是两件事，与 `schema.toml` 的 `sections`
///    说的是同一件事的两种表示（决策 12 的"没看"vs"没有"）。
#[derive(Debug, Serialize)]
pub struct CaptureView {
    /// 写到哪个目录 —— **用户给的那个写法**（相对路径就还是相对路径）。
    #[serde(rename = "outDir")]
    pub out_dir: String,
    /// 捕获时间（RFC 3339，UTC）。**两次运行之间唯一会变的东西。**
    #[serde(rename = "capturedAt")]
    pub captured_at: String,
    /// **`tuoen.d/` 的磁盘格式版本**（`tuoen_core::capture::SCHEMA_VERSION`）。
    ///
    /// 它与信封自己的 `schemaVersion` 是**两件事**：信封那个管 JSON 的形状，
    /// 这个管 TOML 文件的形状；递增的时机不同，碰巧现在都是 1。
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    /// 这份快照包含哪些 section（稳定 slug，**排序后**）。与 `schema.toml` 逐字一致。
    pub sections: Vec<String>,
    /// 这一次写出去的文件名（相对 `outDir`），顺序即写入顺序。
    pub files: Vec<String>,
    /// 工具。`null` = 这次没捕获 `tools`。
    pub tools: Option<ToolsCounts>,
    /// `PATH`。`null` = 这次没捕获 `path`。
    pub path: Option<PathCounts>,
    /// 持久环境变量。`null` = 这次没捕获 `env`。
    pub env: Option<EnvCounts>,
    /// WSL。`null` = 这次没捕获 `wsl`。
    pub wsl: Option<WslCounts>,
    /// 全局包清单。`null` = 这次没捕获 `globals`。
    pub globals: Option<GlobalsCounts>,
    /// 配置文件清单。`null` = 这次没捕获 `configs`。
    pub configs: Option<ConfigsCounts>,
    /// 跳过清单。**`null` 与 `[]` 是两件事**：前者是"这次没扫环境变量，
    /// 「跳过了什么」无从谈起"，后者是"扫了，一条都没跳过"。
    pub skipped: Option<Vec<SkippedView>>,
}

impl CaptureView {
    /// 从 bundle 组装。`opts` 只用来回显 `outDir`（那是用户的输入，不在 bundle 里）。
    #[must_use]
    pub fn new(bundle: &CaptureBundle, opts: &CaptureOptions) -> Self {
        Self {
            out_dir: opts.out_dir.display().to_string(),
            // 时间戳与 section 列表**从 bundle 里拿**：报告说的就是磁盘上那一份。
            captured_at: bundle.schema.captured_at.clone(),
            schema_version: bundle.schema.schema_version,
            sections: bundle.schema.sections.clone(),
            files: bundle
                .file_names()
                .into_iter()
                .map(ToOwned::to_owned)
                .collect(),
            tools: bundle.tools.as_ref().map(ToolsCounts::from),
            path: bundle.path.as_ref().map(PathCounts::from),
            env: bundle.env.as_ref().map(EnvCounts::from),
            wsl: bundle.wsl.as_ref().map(WslCounts::from),
            globals: bundle.globals.as_ref().map(GlobalsCounts::from),
            configs: bundle.configs.as_ref().map(ConfigsCounts::from),
            skipped: bundle
                .skipped
                .as_ref()
                .map(|file| file.skipped.iter().map(SkippedView::from).collect()),
        }
    }
}

/// 失败载荷：**只带我们确实知道的事实**。
///
/// 它刻意没有 `files` 键 —— 理由写在 [`report_failure`] 的文档里。
#[derive(Debug, Serialize)]
pub struct CaptureAttemptView {
    /// 本来要写到哪个目录。
    #[serde(rename = "outDir")]
    pub out_dir: String,
    /// 捕获时间（捕获那一步是成功的，所以这个时间是真的）。
    #[serde(rename = "capturedAt")]
    pub captured_at: String,
    /// 这一次**打算**捕获的 section（`--only` 的映射结果，排序后）。
    pub sections: Vec<&'static str>,
}

impl CaptureAttemptView {
    #[must_use]
    fn new(opts: &CaptureOptions) -> Self {
        Self {
            out_dir: opts.out_dir.display().to_string(),
            captured_at: opts.captured_at.clone(),
            sections: opts
                .effective_sections()
                .iter()
                .map(|section| section.as_str())
                .collect(),
        }
    }
}

/// `tools.toml` 的计数。
#[derive(Debug, Serialize)]
pub struct ToolsCounts {
    /// 条目数。
    pub entries: usize,
    /// 按置信度分组，**按 slug 升序**（顺序稳定才能比逐字节）。
    #[serde(rename = "byConfidence")]
    pub by_confidence: Vec<ConfidenceCount>,
}

/// 一个置信度的计数。**取值是 `crates/core/src/detect.rs` 的七个稳定 slug。**
#[derive(Debug, Serialize)]
pub struct ConfidenceCount {
    /// 稳定 slug（`managed` / `executable` / …），**不本地化**。
    pub confidence: String,
    /// 这个置信度有几条。
    pub count: usize,
}

impl From<&ToolsFile> for ToolsCounts {
    fn from(file: &ToolsFile) -> Self {
        // `BTreeMap`：插入顺序不影响输出顺序，于是 `--json` 逐字节稳定。
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in &file.tool {
            *counts.entry(row.confidence.clone()).or_default() += 1;
        }
        Self {
            entries: file.tool.len(),
            by_confidence: counts
                .into_iter()
                .map(|(confidence, count)| ConfidenceCount { confidence, count })
                .collect(),
        }
    }
}

/// `path.toml` 的计数。
///
/// 这几个数字就是票据要的那几个：两个作用域的原文长度、生效长度、距悬崖还剩多少、
/// 档位、条目数（按作用域分）、重复条目数。
#[derive(Debug, Serialize)]
pub struct PathCounts {
    /// 用户级注册表原文的字符数（未展开）。
    #[serde(rename = "rawUserChars")]
    pub raw_user_chars: usize,
    /// 机器级注册表原文的字符数（未展开）。
    #[serde(rename = "rawMachineChars")]
    pub raw_machine_chars: usize,
    /// 真实生效的那条 `PATH` 的字符数（进程口径，含注入项）。
    #[serde(rename = "effectiveChars")]
    pub effective_chars: usize,
    /// `cmd.exe` 的悬崖（8191 字符 —— 超过它整条 `PATH` 一起失效）。
    pub cliff: usize,
    /// 距悬崖还剩多少字符（超限时为 0，不做回绕）。
    pub remaining: usize,
    /// 稳定 slug：`ok` / `warning` / `critical` / `exceeded`。
    pub level: String,
    /// 条目数（**含空段** —— 空段真的占着位置与长度）。
    pub entries: usize,
    /// 按作用域分组，**固定顺序**：machine / user / process-only。
    #[serde(rename = "byScope")]
    pub by_scope: Vec<ScopeCount>,
    /// **重复条目数**：`dup_index > 0` 的条数。
    ///
    /// 它的可信度**完全取决于 `dup_index` 的语义** —— `path.toml` 的字段说明写的是
    /// "同一个值在这个快照里第几次出现（0 = 首次）"，这个数字就是按那个语义数的。
    /// 若采集器把这个字段改成别的意思（比如"去重分组编号"），这个数字就不再是
    /// "重复了几条" —— 那时要改的是**这里**，而不是报告里的措辞。
    pub duplicates: usize,
}

/// 一个作用域的计数。
#[derive(Debug, Serialize)]
pub struct ScopeCount {
    /// 稳定 slug（`machine` / `user` / `process-only`），**不本地化**。
    /// 中文只在人类输出里 —— 那份名字取自 [`crate::path_view::scope_label`]。
    pub scope: &'static str,
    /// 这个作用域有几条。
    pub count: usize,
}

impl From<&PathFile> for PathCounts {
    fn from(file: &PathFile) -> Self {
        let count = |wanted: EnvScope| file.entry.iter().filter(|row| row.scope == wanted).count();
        Self {
            raw_user_chars: file.budget.raw_user_chars,
            raw_machine_chars: file.budget.raw_machine_chars,
            effective_chars: file.budget.effective_chars,
            cliff: file.budget.cliff,
            remaining: file.budget.remaining,
            level: file.budget.level.clone(),
            entries: file.entry.len(),
            by_scope: [EnvScope::Machine, EnvScope::User, EnvScope::ProcessOnly]
                .into_iter()
                .map(|scope| ScopeCount {
                    scope: scope.as_str(),
                    count: count(scope),
                })
                .collect(),
            duplicates: file.entry.iter().filter(|row| row.dup_index > 0).count(),
        }
    }
}

/// `env.toml` 的计数。
#[derive(Debug, Serialize)]
pub struct EnvCounts {
    /// 变量数（用户级 + 机器级）。
    pub total: usize,
    /// 用户级。
    pub user: usize,
    /// 机器级。
    pub machine: usize,
}

impl From<&EnvFile> for EnvCounts {
    fn from(file: &EnvFile) -> Self {
        let count = |wanted: EnvScope| file.var.iter().filter(|row| row.scope == wanted).count();
        Self {
            total: file.var.len(),
            user: count(EnvScope::User),
            machine: count(EnvScope::Machine),
        }
    }
}

/// `wsl.toml` 的计数。
#[derive(Debug, Serialize)]
pub struct WslCounts {
    /// 发行版数。
    pub distributions: usize,
    /// `BasePath` 不在默认位置（`%LOCALAPPDATA%` 底下）的发行版数 ——
    /// 换机器时最需要知道的那一类。
    #[serde(rename = "nonStandardPath")]
    pub non_standard_path: usize,
    /// vhdx **看过了、不在**的发行版数（`Existence::No`；"不知道"不算）。
    #[serde(rename = "vhdxMissing")]
    pub vhdx_missing: usize,
}

impl From<&WslFile> for WslCounts {
    fn from(file: &WslFile) -> Self {
        Self {
            distributions: file.distribution.len(),
            non_standard_path: file
                .distribution
                .iter()
                .filter(|row| row.non_standard_path)
                .count(),
            vhdx_missing: file
                .distribution
                .iter()
                .filter(|row| row.vhdx_exists == Existence::No)
                .count(),
        }
    }
}

/// `globals.toml` 的计数（决策 167–175）。
///
/// # 为什么"几个包"这个数字必须来自工具自己的回答
///
/// 自己走 `<prefix>\node_modules` 也能数出一个数字，但**本机实测它只数到 5 个，
/// 而工具说 7 个**（少掉的是 scope 目录）。那个假数字在迁移时的表现是
/// "新机器上少装了两个包"，没有人会察觉 —— 所以这份计数里没有"我们自己数的包"。
#[derive(Debug, Serialize)]
pub struct GlobalsCounts {
    /// 有几个工具回答了（一行一个工具）。**枚举失败的工具也算**：
    /// 失败的是"问到几个包"，不是"有没有这个工具"（决策 175）。
    ///
    /// # 为什么它仍然叫 `tools` 而不是"几行"
    ///
    /// ticket #23 之后一个工具可能有**两行**（`machine` + `tuoen`），而这个数字从
    /// #17 起就是"一个工具一行"。它的语义**一个字节都没变**：一行是一个工具。
    /// "有几行"是 `byTool` 的长度，那才是新问题的答案。
    pub tools: usize,
    /// 包总数（各行的 `packages` 相加）。
    pub packages: usize,
    /// 每个工具一行，**按工具名升序**（顺序稳定才能比逐字节）。
    #[serde(rename = "byTool")]
    pub by_tool: Vec<GlobalsRow>,
    /// 枚举失败的行（工具名 + 来源 + 稳定 slug）。
    #[serde(rename = "enumerateErrors")]
    pub enumerate_errors: Vec<EnumerateFailure>,
    /// 全局前缀落在**按版本隔离**目录里的行数（决策 173 的那四支判据）。
    ///
    /// 它值得一个数字：切一个 Node 版本会**静默隐藏**这些包 ——
    /// "清单是齐的"这句话在那个前提下不成立。
    ///
    /// # 这个键的语义从 #17 起**一个字节都没变**
    ///
    /// 它一直是"有多少行满足那条判据"。ticket #23 之后行变多了（一个工具最多两行），
    /// 于是这个数也跟着变大 —— 而那正是实情：tuoen 自己的 npm 根**故意**按运行时
    /// 版本分目录（`…\globals\npm\v24.19.0`，决策 26），它天然满足那条判据。
    /// 改它的含义（例如只数 `machine`）会是一次**改语义**的变更，那要递增
    /// `schemaVersion`（决策 166）—— 所以那件事由下面那个**新键**承担。
    #[serde(rename = "insideVersionDir")]
    pub inside_version_dir: usize,
    /// 上面那个数字里**属于机器自己那一份**的行数。
    ///
    /// 这个键存在的唯一理由：`insideVersionDir` 那句话原本的意思是"**别人**
    /// （版本管理器）会把它换掉，所以这份清单只对当下这个版本成立"。两个含义
    /// 混在一个数里，那句警告就从"提醒"变成"噪声" —— 于是谁想知道"我该担心几行"，
    /// 看这个键；谁想知道"两边的总数"，看上面那个。
    #[serde(rename = "insideVersionDirMachine")]
    pub inside_version_dir_machine: usize,
}

/// 一个工具的包数（`globals.toml` 的一行）。
#[derive(Debug, Serialize)]
pub struct GlobalsRow {
    /// 工具名（稳定 slug：`npm` / `pip`）。
    pub tool: String,
    /// 这一行是谁的清单（稳定 slug：`machine` / `tuoen`）。
    pub source: String,
    /// 运行时版本，**原样**（`v24.19.0` / `3.12` / `unknown`）。
    #[serde(rename = "toolVersion")]
    pub tool_version: String,
    /// 这个工具答了几个包。
    pub packages: usize,
}

/// 一个枚举失败的行。`error` 是稳定 slug（`command-failed` / `timed-out` /
/// `bad-json` / `unsupported-output`）。
#[derive(Debug, Serialize)]
pub struct EnumerateFailure {
    /// 哪个工具。
    pub tool: String,
    /// 哪个来源的那一行失败了（`machine` / `tuoen`）。
    ///
    /// 没有它的话，"npm 枚举失败"在两个来源下是同一句话，而修法完全不同：
    /// 机器那一份失败要看用户的 npm 配置，我们那一份失败要看
    /// `%LOCALAPPDATA%\tuoen\globals` 的权限。
    pub source: String,
    /// 失败的原因 slug。
    pub error: String,
}

impl From<&GlobalsFile> for GlobalsCounts {
    fn from(file: &GlobalsFile) -> Self {
        Self {
            tools: file.global.len(),
            packages: file.global.iter().map(|row| row.packages.len()).sum(),
            by_tool: file
                .global
                .iter()
                .map(|row| GlobalsRow {
                    tool: row.tool.clone(),
                    source: row.source.clone(),
                    tool_version: row.tool_version.clone(),
                    packages: row.packages.len(),
                })
                .collect(),
            enumerate_errors: file
                .global
                .iter()
                .filter_map(|row| {
                    row.enumerate_error.as_ref().map(|error| EnumerateFailure {
                        tool: row.tool.clone(),
                        source: row.source.clone(),
                        error: error.clone(),
                    })
                })
                .collect(),
            inside_version_dir: file
                .global
                .iter()
                .filter(|row| row.prefix_inside_version_dir == Some(true))
                .count(),
            inside_version_dir_machine: file
                .global
                .iter()
                .filter(|row| {
                    row.source == tuoen_core::GlobalsSource::Machine.slug()
                        && row.prefix_inside_version_dir == Some(true)
                })
                .count(),
        }
    }
}

/// `configs.toml` 的计数（决策 176–184）。
#[derive(Debug, Serialize)]
pub struct ConfigsCounts {
    /// 候选总数（配置文件 + JetBrains 产品目录的标记行）。
    pub entries: usize,
    /// 读到了、算出了哈希的条数。
    pub captured: usize,
    /// 没捕获的条数。**每一条都在 `skipped.toml` 里**，理由具体（决策 179）。
    pub skipped: usize,
    /// 按 `kind` 分组，**按 slug 升序**。
    #[serde(rename = "byKind")]
    pub by_kind: Vec<KindCount>,
    /// Git 身份来自哪一层（`system` / `global` / `missing` / `unknown`）。
    ///
    /// **`missing` 与 `unknown` 是两件事**（决策 183）：前者是"问了，两层都没有
    /// 身份"，后者是"我们**问不了**"（这台机器上没有可用的 `git`）。整张表缺席
    /// 会让"没问"与"没装 git"长得一样，所以它是 `Option` 而不是空串。
    #[serde(rename = "gitIdentitySource", skip_serializing_if = "Option::is_none")]
    pub git_identity_source: Option<String>,
}

/// 一个 `kind` 的计数。
#[derive(Debug, Serialize)]
pub struct KindCount {
    /// 稳定 slug（`git` / `npm` / `maven` / `docker` / `ssh` / `wsl` / `vscode` /
    /// `jetbrains`），**不本地化**。
    pub kind: String,
    /// 这个类别有几条。
    pub count: usize,
}

impl From<&ConfigsFile> for ConfigsCounts {
    fn from(file: &ConfigsFile) -> Self {
        // `BTreeMap`：插入顺序不影响输出顺序，于是 `--json` 逐字节稳定。
        let mut counts: BTreeMap<String, usize> = BTreeMap::new();
        for row in &file.config {
            *counts.entry(row.kind.clone()).or_default() += 1;
        }
        Self {
            entries: file.config.len(),
            captured: file.config.iter().filter(|row| row.captured).count(),
            skipped: file.config.iter().filter(|row| !row.captured).count(),
            by_kind: counts
                .into_iter()
                .map(|(kind, count)| KindCount { kind, count })
                .collect(),
            git_identity_source: file.git.as_ref().map(|git| git.identity_source.clone()),
        }
    }
}

/// 一条跳过项。
///
/// **它是"我们看见了、但故意没写"的唯一出口。** 这里只有名字、位置与原因，
/// 没有任何材料：值、前几位、长度、哈希都不许可（见 `capture::secrets` 与
/// `files.rs` 的 `SkippedFile` 文档）。
#[derive(Debug, Serialize)]
pub struct SkippedView {
    /// 属于哪个 section（`env` / `configs`）。
    pub section: String,
    /// 在哪个作用域里看到的（环境变量用）。文件类的跳过项是 `null`。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// **名字**，不是值。（它是机器给的，不是我们写的文本。）
    pub name: String,
    /// 为什么跳过：稳定 slug（`credential` / `unreadable` / …）。
    /// **脚本要分支就分这个** —— 它与中文说明分开，所以说明可以改，判据不能。
    pub kind: String,
    /// 一句话原因，**中文**。
    ///
    /// **它不进 JSON**（`serde(skip)`）：成功载荷里不许有中文（决策 35），
    /// 而这一句天生就是中文。判据已经是稳定 slug 了，脚本不需要这句人话；
    /// 需要人话的是**人**，而人看的是人类输出 —— 两条出口各取所需。
    #[serde(skip)]
    pub reason: String,
}

impl From<&SkipEntry> for SkippedView {
    fn from(entry: &SkipEntry) -> Self {
        Self {
            section: entry.section.clone(),
            scope: entry.scope.clone(),
            name: entry.name.clone(),
            kind: entry.kind.clone(),
            reason: entry.reason.clone(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 人类输出（中文优先）
// ─────────────────────────────────────────────────────────────────────────────

/// `tuoen capture` 的人类输出。
///
/// 三段：写到哪（含逐文件清单）、每类内容的关键数字、下一步。
///
/// **跳过那一段不许省** —— 有跳过项就逐条列出来（名字 + 种类 + 原因），
/// 一条都没有也要说一句"没有跳过任何东西"。**静默跳过是 bug**：
/// 它会让"都备份好了"变成一句假话，而用户永远不会知道少了什么。
pub fn print_human(view: &CaptureView, no_version: bool) {
    println!("写到：{}", view.out_dir);
    println!("捕获时间：{}", view.captured_at);
    println!(
        "文件 {} 个（每类内容一个文件 —— 它们各自是一个独立的 diff）：",
        view.files.len()
    );
    for name in &view.files {
        println!("  · {name}");
    }
    println!("  （这些文件是**给人提交进仓库**的：换机器时它们就是你重建环境的依据，");
    println!("    所以 `skipped.toml` 也在里面。）");
    println!();

    print_path_section(view.path.as_ref());
    print_tools_section(view.tools.as_ref());
    print_env_section(view.env.as_ref());
    print_wsl_section(view.wsl.as_ref());
    print_globals_section(view.globals.as_ref());
    print_configs_section(view.configs.as_ref());
    print_skipped_section(view.skipped.as_deref());

    println!("（本次**只读**：没有写 `PATH`、没有写注册表、没有动任何工具。）");
    if no_version {
        println!("（本次没有探测版本 —— 加了 `--no-version`，所以 `tools.toml` 里没有版本号。）");
    }
    println!();
    println!("下一步：");
    println!(
        "  · 把 `{}` 提交进仓库 —— 它是可 diff 的，也是将来 `restore` 的输入。",
        view.out_dir
    );
    println!("  · 想知道这台机器哪里是坏的：`tuoen doctor`（那是下一张票，还没实现）。");
}

/// `path.toml` 那一段。
fn print_path_section(path: Option<&PathCounts>) {
    let Some(path) = path else {
        println!("path.toml —— 这次没捕获 `path`（`--only` 里没有它）。");
        println!();
        return;
    };
    // 作用域的中文名只有一份定义（`path_view::scope_label`）—— 在这里再写一遍
    // 意味着某一天 `path show` 与 `capture` 会用两个名字说同一个东西。
    let by_scope = path
        .by_scope
        .iter()
        .map(|row| format!("{} {}", crate::path_view::scope_label(row.scope), row.count))
        .collect::<Vec<_>>()
        .join(" · ");
    println!("path.toml —— `PATH` 的结构");
    println!(
        "  原文：用户级 {} 字符 · 机器级 {} 字符；生效 {} 字符（进程口径，含进程注入项）",
        path.raw_user_chars, path.raw_machine_chars, path.effective_chars
    );
    println!(
        "  距 {} 的悬崖还剩 {} 字符（档位 {}）—— 超过悬崖后 `cmd.exe` 会**完全忽略整条 `PATH`**。",
        path.cliff, path.remaining, path.level
    );
    println!("  条目 {} 条：{by_scope}", path.entries);
    if path.duplicates == 0 {
        println!("  重复条目：没有。");
    } else {
        // 判据就是票据要的那一条：`dup_index > 0` 的条数
        // （`dup_index` 的语义见 `path.toml` 的字段说明：0 = 首次出现）。
        println!(
            "  重复条目 {} 条（按 `dup_index > 0` 数 —— 同一个值在这个快照里出现了不止一次）。",
            path.duplicates
        );
        println!("  （tuoen **只报告，不自动清**：`PATH` 上的东西是你的，决策 24。）");
    }
    println!();
}

/// `tools.toml` 那一段。
fn print_tools_section(tools: Option<&ToolsCounts>) {
    let Some(tools) = tools else {
        println!("tools.toml —— 这次没捕获 `tools`（`--only` 里没有它）。");
        println!();
        return;
    };
    println!("tools.toml —— 工具与版本");
    println!("  条目 {} 条。", tools.entries);
    if tools.by_confidence.is_empty() {
        println!("  （一台工具都没检测到 —— 这不常见，请把这份输出报告给我们。）");
    } else {
        let by_confidence = tools
            .by_confidence
            .iter()
            .map(|row| format!("{} {}", row.confidence, row.count))
            .collect::<Vec<_>>()
            .join(" · ");
        println!("  置信度：{by_confidence}");
    }
    println!();
}

/// `env.toml` 那一段。
fn print_env_section(env: Option<&EnvCounts>) {
    let Some(env) = env else {
        println!("env.toml —— 这次没捕获 `env`（`--only` 里没有它）。");
        println!();
        return;
    };
    println!("env.toml —— 持久环境变量（**只有**用户级与机器级）");
    println!(
        "  变量 {} 个：用户级 {} · 机器级 {}",
        env.total, env.user, env.machine
    );
    // 落在这两个作用域之外的行会让上面那句话自相矛盾（总数对不上分量），
    // 所以它必须被说出来，而不是让读者自己去算。
    let other = env.total.saturating_sub(env.user + env.machine);
    if other > 0 {
        println!("  ⚠ 另有 {other} 条落在别的作用域 —— 这不该发生，请报告。");
    }
    println!("  （进程环境不写：它是「这一台机器此刻的结果」，不是可搬运的状态。）");
    println!();
}

/// `wsl.toml` 那一段。
fn print_wsl_section(wsl: Option<&WslCounts>) {
    let Some(wsl) = wsl else {
        println!("wsl.toml —— 这次没捕获 `wsl`（`--only` 里没有它）。");
        println!();
        return;
    };
    println!("wsl.toml —— WSL 发行版与它们**实际**的 vhdx 路径");
    println!(
        "  发行版 {} 个（{} 个不在默认位置，{} 个找不到 vhdx）。",
        wsl.distributions, wsl.non_standard_path, wsl.vhdx_missing
    );
    println!();
}

/// `globals.toml` 那一段。
///
/// 四句话是这个 section 的全部价值：**有几个包**、**它们挂在哪个运行时版本下**、
/// **它们是哪个来源的**、**哪个工具没答上来**。最后一句最容易被省掉 ——
/// 省掉之后"清单是齐的"就是一句无法验证的话（决策 175 要的正是"行照样写出去，
/// 失败照样说出来"）。
fn print_globals_section(globals: Option<&GlobalsCounts>) {
    let Some(globals) = globals else {
        println!("globals.toml —— 这次没捕获 `globals`（`--only` 里没有它）。");
        println!();
        return;
    };
    println!("globals.toml —— 全局包清单（**只来自工具自己的回答**）");
    if globals.tools == 0 {
        println!("  一个工具都没回答 —— `npm` / `pip` 都不在这台机器的 `PATH` 上。");
        println!("  （这不是错误：找不到可执行文件的工具**不产生行**，决策 171。）");
        println!();
        return;
    }
    println!("  清单 {} 行，包 {} 个：", globals.tools, globals.packages);
    for row in &globals.by_tool {
        println!(
            "  · {} [{}] {} —— {} 个包",
            row.tool, row.source, row.tool_version, row.packages
        );
    }
    // "按版本隔离"那句话**按来源拆开**（ticket #23）：机器那一份是警告，
    // 我们那一份是设计 —— 把两者印成同一句话，警告就变成了噪声。
    if globals.inside_version_dir_machine > 0 {
        println!(
            "  ⚠ 机器自己的清单里有 {} 个工具的全局前缀落在**按版本隔离**的目录里：",
            globals.inside_version_dir_machine
        );
        println!(
            "    换一个运行时版本，这些包会被**静默隐藏**（它们还装着，只是不在那个版本下）。"
        );
    }
    if globals.inside_version_dir > globals.inside_version_dir_machine {
        println!("  · tuoen 自己的根落在按版本隔离的目录里 —— 那是**设计**（决策 26）：");
        println!(
            "    npm 的根是 `<base>\\npm\\<node -v 原样>`，所以两个 Node 版本各有一套全局包，互不污染。"
        );
    }
    if globals.enumerate_errors.is_empty() {
        println!("  枚举失败：没有。");
    } else {
        for failure in &globals.enumerate_errors {
            println!(
                "  ⚠ {} [{}] 枚举失败（{}）—— 这一行没有包清单，但工具本身在。",
                failure.tool, failure.source, failure.error
            );
        }
    }
    println!("  （自己走 `<prefix>\\node_modules` 数出来的数字**不算**：实测它少 2 个。）");
    println!();
}

/// `configs.toml` 那一段。
///
/// 最后一句不是客套：这份清单里**只有路径与哈希**，一个字的内容都没有 ——
/// 而"跳过的那几条去哪了"必须当场回答（`skipped.toml`），否则
/// 「配置文件都备份好了」会是一句假话。
fn print_configs_section(configs: Option<&ConfigsCounts>) {
    let Some(configs) = configs else {
        println!("configs.toml —— 这次没捕获 `configs`（`--only` 里没有它）。");
        println!();
        return;
    };
    println!("configs.toml —— 配置文件清单（**只有路径与哈希，没有内容**）");
    println!(
        "  候选 {} 条：捕获 {} · 跳过 {}",
        configs.entries, configs.captured, configs.skipped
    );
    if configs.by_kind.is_empty() {
        println!("  （一个候选文件都没找到 —— 这台机器上这些配置文件都不在默认位置。）");
    } else {
        let by_kind = configs
            .by_kind
            .iter()
            .map(|row| format!("{} {}", row.kind, row.count))
            .collect::<Vec<_>>()
            .join(" · ");
        println!("  按类别：{by_kind}");
    }
    match configs.git_identity_source.as_deref() {
        // 决策 183：**跨层级读**。只看 `~/.gitconfig` 会丢掉整个身份
        // （本机两层都没有身份，而系统级有 11 个键）。
        Some("system") => println!("  git 身份：来自**系统级** gitconfig。"),
        Some("global") => println!("  git 身份：来自**全局级** gitconfig。"),
        Some("missing") => {
            println!("  git 身份：两层都读了，**都没有** `user.name` / `user.email`。")
        }
        Some("unknown") => {
            println!("  git 身份：**问不到** —— 这台机器上没有可用的 `git`（不是「没有身份」）。")
        }
        Some(other) => println!("  git 身份：{other}（这个取值我没见过，请报告）。"),
        None => println!("  git 身份：这次没有问（没有 `git` 那张表）。"),
    }
    if configs.skipped > 0 {
        println!(
            "  跳过的那 {} 条在 `skipped.toml` 里，逐条带原因（含形似凭据的文件）。",
            configs.skipped
        );
    }
    println!("  （快照里**没有**任何配置文件的正文 —— 这是设计：`tuoen.d/` 要进仓库。）");
    println!();
}

/// `skipped.toml` 那一段。**三种情形三句不同的话**（见本函数的 `match`）。
fn print_skipped_section(skipped: Option<&[SkippedView]>) {
    println!("skipped.toml —— 看见了但**故意没有写进快照**的东西");
    match skipped {
        // `None` 与"零条"必须分开说：前者是"没扫过"，后者是"扫了，没有东西被跳过"。
        // 混成一句会让"这份快照是完整的"变成一句无法验证的话。
        None => {
            println!(
                "  这次既没有扫环境变量、也没有扫配置文件（`--only` 里没有 `env` 与 `configs`），\
                 所以**没有跳过清单** ——"
            );
            println!("  「跳过了什么」这句话只在真的扫过之后才有意义。");
        }
        Some([]) => println!("  没有跳过任何东西 —— 扫到的都写进去了。"),
        Some(entries) => {
            println!(
                "  跳过了 {} 条（**它们没有被写进任何文件**）：",
                entries.len()
            );
            for entry in entries {
                println!(
                    "    · {}（{} / {}）—— {}：{}",
                    entry.name,
                    entry.section,
                    entry.scope.as_deref().unwrap_or("-"),
                    entry.kind,
                    entry.reason
                );
            }
        }
    }
    println!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use tuoen_core::capture::{
        PathBudgetRow, PathFile, SCHEMA_VERSION, SchemaFile, Section, SkippedFile, WslFile,
    };

    fn has_cjk(text: &str) -> bool {
        text.chars()
            .any(|c| (0x4E00..=0x9FFF).contains(&(u32::from(c))))
    }

    fn a_budget() -> PathBudgetRow {
        PathBudgetRow {
            raw_user_chars: 773,
            raw_machine_chars: 1007,
            effective_chars: 1781,
            cliff: 8191,
            remaining: 6410,
            level: "ok".to_owned(),
        }
    }

    /// 一台"什么都没捕获"的机器 —— 形状必须完整，消费者不该为干净机器写特例。
    fn a_bundle() -> CaptureBundle {
        CaptureBundle {
            schema: SchemaFile::new(
                "2026-10-02T12:00:00Z",
                "0.1.0",
                vec!["env".to_owned(), "path".to_owned()],
            ),
            tools: None,
            path: Some(PathFile::new("2026-10-02T12:00:00Z", a_budget())),
            env: Some(EnvFile::new("2026-10-02T12:00:00Z")),
            wsl: Some(WslFile::new("2026-10-02T12:00:00Z")),
            globals: None,
            configs: None,
            skipped: None,
        }
    }

    #[test]
    fn a_skipped_entry_serialises_without_its_chinese_reason() {
        // **这是 `--json` 里 `skipped[]` 的形状**，也是"中文不进成功载荷"这条
        // 决策在跳过清单上的落点：判据是 `kind` 那个 slug，人话只在人类输出里。
        let mut skipped = SkippedFile::new("2026-10-02T12:00:00Z");
        skipped.push(SkipEntry {
            section: "env".to_owned(),
            scope: Some("user".to_owned()),
            name: "GITLAB_TOKEN".to_owned(),
            kind: "credential".to_owned(),
            reason: "值形似 GitLab 访问令牌".to_owned(),
        });
        let view = SkippedView::from(&skipped.skipped[0]);
        let json = serde_json::to_string(&view).expect("序列化");
        assert_eq!(
            json,
            r#"{"section":"env","scope":"user","name":"GITLAB_TOKEN","kind":"credential"}"#
        );
        assert!(!has_cjk(&json), "成功载荷里不该有中文：{json}");
        // 中文那句**还在**（人类输出要用它）—— 被去掉的只是它在 JSON 里的位置。
        assert!(has_cjk(&view.reason));
    }

    #[test]
    fn the_payload_has_every_promised_key_even_when_a_section_was_not_captured() {
        let opts = CaptureOptions::only("out", "2026-10-02T12:00:00Z", vec![Section::Path]);
        let view = CaptureView::new(&a_bundle(), &opts);
        let json = serde_json::to_string(&view).expect("序列化");

        for key in [
            "\"outDir\":",
            "\"capturedAt\":",
            "\"schemaVersion\":1",
            "\"sections\":[\"env\",\"path\"]",
            // 顺序就是 `file_names()` 的顺序（= 写入顺序）：`tools` 是 `None`，
            // 所以 `tools.toml` 连文件名都不出现 —— 那是"没捕获"，不是"没有"。
            "\"files\":[\"schema.toml\",\"path.toml\",\"env.toml\",\"wsl.toml\"]",
            "\"tools\":null",
            "\"path\":",
            "\"env\":",
            "\"wsl\":",
            "\"skipped\":null",
        ] {
            assert!(json.contains(key), "缺少 {key}：{json}");
        }
        assert_eq!(view.schema_version, SCHEMA_VERSION);
        // 一台空机器上的计数必须是 0，而不是缺席 —— 0 与"没说"是两件事。
        assert_eq!(view.path.as_ref().expect("捕获了 path").entries, 0);
        assert_eq!(view.env.as_ref().expect("捕获了 env").total, 0);
        assert_eq!(view.wsl.as_ref().expect("捕获了 wsl").distributions, 0);
        // 我们写的字符串里没有中文（`outDir` 是用户的输入，不在这条断言的范围里）。
        assert!(!has_cjk(&view.sections.join(",")));
        assert!(!has_cjk(&view.files.join(",")));
        assert!(!has_cjk(&view.path.as_ref().expect("path").level));
        // 三个作用域**固定顺序、0 也在**：消费者不该为"这台机器上没有进程注入项"
        // 写一条特殊分支。
        let scopes: Vec<(&str, usize)> = view
            .path
            .as_ref()
            .expect("path")
            .by_scope
            .iter()
            .map(|row| (row.scope, row.count))
            .collect();
        assert_eq!(
            scopes,
            vec![("machine", 0), ("user", 0), ("process-only", 0)]
        );
    }

    #[test]
    fn the_path_counts_come_from_the_budget_and_the_rows() {
        let mut file = PathFile::new("2026-10-02T12:00:00Z", a_budget());
        file.entry.push(tuoen_core::capture::PathRow {
            scope: EnvScope::Machine,
            index: 0,
            owner: "system".to_owned(),
            raw: r"C:\Windows".to_owned(),
            expanded: r"C:\Windows".to_owned(),
            quoted: false,
            empty: false,
            reg_type: None,
            exists: Existence::Yes,
            reparse: tuoen_platform::ReparseKind::None,
            link_target: None,
            has_vars: false,
            has_username: false,
            dup_index: 0,
        });
        file.entry.push(tuoen_core::capture::PathRow {
            scope: EnvScope::User,
            index: 0,
            owner: "unknown".to_owned(),
            raw: r"C:\tools".to_owned(),
            expanded: r"C:\tools".to_owned(),
            quoted: false,
            empty: false,
            reg_type: None,
            exists: Existence::Yes,
            reparse: tuoen_platform::ReparseKind::None,
            link_target: None,
            has_vars: false,
            has_username: false,
            // 第 2 次出现 —— 这就是"重复条目"的判据。
            dup_index: 1,
        });

        let counts = PathCounts::from(&file);
        assert_eq!(counts.effective_chars, 1781);
        assert_eq!(counts.cliff, 8191);
        assert_eq!(counts.remaining, 6410);
        assert_eq!(counts.entries, 2);
        assert_eq!(counts.duplicates, 1, "`dup_index > 0` 才是重复");
        let by_scope: Vec<(&str, usize)> = counts
            .by_scope
            .iter()
            .map(|row| (row.scope, row.count))
            .collect();
        assert_eq!(
            by_scope,
            vec![("machine", 1), ("user", 1), ("process-only", 0)],
            "作用域的顺序固定，0 也要印出来"
        );
    }

    /// 一个工具一行，包总数是**各行相加**，失败的行照样算一行（决策 175）。
    ///
    /// ticket #23 之后一行还要带 `source`：这份固定装置是"两个来源都在"的形状
    /// （机器那一份两条，tuoen 那一份一条），这样 `insideVersionDir` 与
    /// `insideVersionDirMachine` 的差别才**测得出来**。
    fn a_globals() -> GlobalsFile {
        let mut file = GlobalsFile::new("2026-10-02T12:00:00Z");
        file.global.push(tuoen_core::capture::GlobalRow {
            tool: "npm".to_owned(),
            source: "machine".to_owned(),
            tool_version: "v24.19.0".to_owned(),
            prefix: Some(r"C:\nvm4w\nodejs".to_owned()),
            prefix_inside_version_dir: Some(true),
            packages: vec![
                tuoen_core::capture::GlobalPackage {
                    name: "@deepseek-ai/dsh".to_owned(),
                    version: "1.2.3".to_owned(),
                },
                tuoen_core::capture::GlobalPackage {
                    name: "typescript".to_owned(),
                    version: "5.6.3".to_owned(),
                },
            ],
            enumerate_error: None,
        });
        file.global.push(tuoen_core::capture::GlobalRow {
            tool: "npm".to_owned(),
            source: "tuoen".to_owned(),
            tool_version: "v24.19.0".to_owned(),
            // 我们自己的 npm 根**故意**按版本分目录（决策 26）⇒ 这一支也是 `true`。
            prefix: Some(r"C:\Users\x\AppData\Local\tuoen\globals\npm\v24.19.0".to_owned()),
            prefix_inside_version_dir: Some(true),
            packages: Vec::new(),
            enumerate_error: None,
        });
        file.global.push(tuoen_core::capture::GlobalRow {
            tool: "pip".to_owned(),
            source: "machine".to_owned(),
            tool_version: "unknown".to_owned(),
            // 拿不到前缀 → 两个键**一起不出**（决策 174 的同生共死）。
            prefix: None,
            prefix_inside_version_dir: None,
            packages: Vec::new(),
            enumerate_error: Some("timed-out".to_owned()),
        });
        file
    }

    #[test]
    fn the_globals_counts_add_up_and_name_the_tool_that_did_not_answer() {
        let counts = GlobalsCounts::from(&a_globals());
        assert_eq!(counts.tools, 3, "枚举失败的行**也算一行**（决策 175）");
        assert_eq!(counts.packages, 2, "包总数是各行相加");
        let by_tool: Vec<(&str, &str, usize)> = counts
            .by_tool
            .iter()
            .map(|row| (row.tool.as_str(), row.source.as_str(), row.packages))
            .collect();
        assert_eq!(
            by_tool,
            vec![
                ("npm", "machine", 2),
                ("npm", "tuoen", 0),
                ("pip", "machine", 0)
            ],
            "同一个工具的两个来源是**两行**：来源是行的一部分"
        );
        assert_eq!(
            counts
                .enumerate_errors
                .iter()
                .map(|row| (row.tool.as_str(), row.source.as_str(), row.error.as_str()))
                .collect::<Vec<_>>(),
            vec![("pip", "machine", "timed-out")],
            "没答上来的行必须被点名（带来源）—— 否则「清单是齐的」是一句无法验证的话"
        );
        assert_eq!(
            counts.inside_version_dir, 2,
            "两行都落在按版本隔离的目录里（机器那一份 + 我们那一份）"
        );
        assert_eq!(
            counts.inside_version_dir_machine, 1,
            "**只有 machine 那一行**是「会被版本管理器换掉」的警告 —— tuoen 那一行是设计"
        );

        // 序列化之后键名是 camelCase，且**新键一个都不许带中文**。
        let json = serde_json::to_string(&counts).expect("序列化");
        for key in [
            "\"tools\":3",
            "\"packages\":2",
            "\"byTool\":",
            "\"toolVersion\":\"v24.19.0\"",
            "\"source\":\"tuoen\"",
            "\"enumerateErrors\":[{\"tool\":\"pip\",\"source\":\"machine\",\"error\":\"timed-out\"}]",
            "\"insideVersionDir\":2",
            "\"insideVersionDirMachine\":1",
        ] {
            assert!(json.contains(key), "缺少 {key}：{json}");
        }
        assert!(!has_cjk(&json), "{json}");
    }

    #[test]
    fn the_configs_counts_split_captured_from_skipped_and_carry_the_git_source() {
        let mut file = ConfigsFile::new("2026-10-02T12:00:00Z");
        file.config.push(tuoen_core::capture::ConfigRow {
            path: r"C:\Users\x\.gitconfig".to_owned(),
            kind: "git".to_owned(),
            layer: Some("global".to_owned()),
            captured: true,
            bytes: Some(142),
            content_hash: Some("sha256:abc".to_owned()),
            skip_reason: None,
        });
        // 决策 184 的**唯一例外**：JetBrains 的目录标记行 `captured = true`
        // 却没有 `bytes` / `content_hash`（目录不是文件）。
        file.config.push(tuoen_core::capture::ConfigRow {
            path: r"C:\Users\x\AppData\Roaming\JetBrains\IntelliJIdea2026.1".to_owned(),
            kind: "jetbrains".to_owned(),
            layer: None,
            captured: true,
            bytes: None,
            content_hash: None,
            skip_reason: None,
        });
        file.config.push(tuoen_core::capture::ConfigRow {
            path: r"C:\Users\x\.m2\settings.xml".to_owned(),
            kind: "maven".to_owned(),
            layer: None,
            captured: false,
            bytes: None,
            content_hash: None,
            skip_reason: Some("contains-credential-shape".to_owned()),
        });
        file.git = Some(tuoen_core::capture::GitFacts {
            identity_source: "missing".to_owned(),
            system_config: Some(r"C:\ProgramData\Git\config".to_owned()),
            global_config: Some(r"C:\Users\x\.gitconfig".to_owned()),
            user_name: None,
            user_email: None,
        });

        let counts = ConfigsCounts::from(&file);
        assert_eq!(counts.entries, 3);
        assert_eq!(counts.captured, 2, "JetBrains 的标记行算捕获");
        assert_eq!(counts.skipped, 1);
        let by_kind: Vec<(&str, usize)> = counts
            .by_kind
            .iter()
            .map(|row| (row.kind.as_str(), row.count))
            .collect();
        assert_eq!(
            by_kind,
            vec![("git", 1), ("jetbrains", 1), ("maven", 1)],
            "按 slug 升序（顺序稳定才能比逐字节）"
        );
        assert_eq!(
            counts.git_identity_source.as_deref(),
            Some("missing"),
            "`missing` 与 `unknown` 是两件事（决策 183）"
        );

        let json = serde_json::to_string(&counts).expect("序列化");
        for key in [
            "\"entries\":3",
            "\"captured\":2",
            "\"skipped\":1",
            "\"byKind\":",
            "\"gitIdentitySource\":\"missing\"",
        ] {
            assert!(json.contains(key), "缺少 {key}：{json}");
        }
        assert!(!has_cjk(&json), "{json}");

        // 没有 `[git]` 表时那个键**不出**（`null` 会被读成"git 说没有身份"）。
        let mut without_git = ConfigsFile::new("2026-10-02T12:00:00Z");
        without_git.config = file.config;
        let json = serde_json::to_string(&ConfigsCounts::from(&without_git)).expect("序列化");
        assert!(
            !json.contains("gitIdentitySource"),
            "问不到 git 时这个键必须缺席：{json}"
        );
    }
}
