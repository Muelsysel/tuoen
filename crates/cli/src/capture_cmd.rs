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
    CaptureBundle, CaptureError, CaptureOptions, EnvFile, Existence, PathFile, SkipEntry,
    ToolsFile, WslFile, capture, write_bundle,
};
use tuoen_platform::EnvScope;
use tuoen_store::Store;

use crate::capture::CaptureArgs;
use crate::envelope::Envelope;
use crate::{exit, managed};

// ─────────────────────────────────────────────────────────────────────────────
// 编排
// ─────────────────────────────────────────────────────────────────────────────

/// 跑 `tuoen capture`。返回退出码。
pub fn run(args: &CaptureArgs) -> i32 {
    // 六个真实依赖，与 `run_detect` 一模一样 —— 捕获**不是**另一套检测逻辑，
    // 它是"把检测结果与其余三样东西写成文件"。两套装配迟早会读出两份不同的机器。
    let fs = tuoen_platform::RealFileSystem;
    let registry = tuoen_platform::RealRegistry;
    let env = tuoen_platform::RealEnvBlock::new(registry, fs);
    let process_env = tuoen_platform::RealProcessEnv::new();
    let runner = tuoen_platform::SystemProcessRunner;
    let managed = managed::StoreManagedStore::at_default_location();
    let scan_roots = tuoen_core::detect::engine::default_scan_roots(&process_env);

    let ctx = tuoen_core::detect::DetectContext {
        fs: &fs,
        registry: &registry,
        env: &env,
        process_env: &process_env,
        runner: &runner,
        managed: &managed,
        probe_timeout: tuoen_platform::DEFAULT_PROBE_TIMEOUT,
        // `--no-version` 只影响**探测**：条目照旧被发现，只是没有版本号。
        probe_versions: !args.no_version,
        scan_roots,
    };

    let store = Store::at_default_location();
    let mut opts = CaptureOptions::all(args.out.clone(), &tuoen_store::now_rfc3339());
    // **空向量就是"全部"**（`CaptureOptions` 的既定语义），所以这里直接转发：
    // 一个忘了传参的调用不该安静地什么都不做。
    opts.sections = args.only.iter().map(|section| section.section()).collect();
    // 我们自己的两个根 —— 它是 `owner = "tuoen"` 的**唯一**判据（决定还原时哪些
    // `PATH` 条目可以动）。shim 目录用 `shim_cmd::shim_dir`（全仓唯一一份定义），
    // 不自己拼路径：第二份定义会漂移，而它漂移的后果是快照里一整类条目的归属出错。
    opts.tuoen_roots = vec![
        store.root().to_path_buf(),
        crate::shim_cmd::shim_dir(&store),
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

/// 一条跳过项。
///
/// **它是"我们看见了、但故意没写"的唯一出口。** 这里只有名字、位置与原因，
/// 没有任何材料：值、前几位、长度、哈希都不许可（见 `capture::secrets` 与
/// `files.rs` 的 `SkippedFile` 文档）。
#[derive(Debug, Serialize)]
pub struct SkippedView {
    /// 属于哪个 section（`env` / 未来的 `configs`）。
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

/// `skipped.toml` 那一段。**三种情形三句不同的话**（见本函数的 `match`）。
fn print_skipped_section(skipped: Option<&[SkippedView]>) {
    println!("skipped.toml —— 看见了但**故意没有写进快照**的东西");
    match skipped {
        // `None` 与"零条"必须分开说：前者是"没扫过"，后者是"扫了，没有东西被跳过"。
        // 混成一句会让"这份快照是完整的"变成一句无法验证的话。
        None => {
            println!("  这次没有扫环境变量（`--only` 里没有 `env`），所以**没有跳过清单** ——");
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
}
