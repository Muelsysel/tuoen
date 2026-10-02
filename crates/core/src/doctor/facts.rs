//! 事实采集 —— `doctor` 里**唯一碰机器的地方**，而且只读。
//!
//! 前四个事实直接来自 `capture` 的采集器（同一个 crate，同一批代码）：
//! `path.toml` / `env.toml` / `tools.toml` / `wsl.toml` 的形状就是事实的形状。
//! 剩下五个是 `doctor` 需要、`capture` 不需要的活事实。
//!
//! # 每一个"活事实"都带一个分母
//!
//! `doctor` 最容易犯的错不是报错，而是**在什么都没看的情况下报 0 条**。所以
//! 事实里带着规模：解析了多少条命令、盘上有几个 shim、扫了几个根目录。
//! 检查项报 0 条时，读的人能从 [`super::FactsSummary`] 里看出那是"没有"还是"没看"。

use std::path::Path;
use std::time::Duration;

use tuoen_platform::{EnvScope, RegHive, RegValue, ReparseKind};

use super::{DoctorOptions, MachineFacts};
use crate::capture::collect;
use crate::capture::files::{PathFile, ToolsFile};
use crate::detect::engine::default_scan_roots;
use crate::detect::{DetectContext, KNOWN_TOOLS, spec};

/// 操作系统事实。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemFacts {
    /// 当前进程有没有管理员权限。`None` = 问不出来（**不是"没提权"**）。
    pub elevated: Option<bool>,
    /// Developer Mode（决定能不能建 symlink）。`None` = 这个键不在。
    pub developer_mode: Option<bool>,
    /// `LongPathsEnabled`。`None` = 这个键不在。
    pub long_paths: Option<bool>,
}

/// 一条命令**解析到了哪个目录**。
///
/// 判"同一个工具的不同命令是不是来自不同安装"只需要目录，不需要版本 ——
/// 版本号不是证据（L0 的真机验收：本机两个 Node 版本号恰好相同）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandResolution {
    /// 逻辑工具名（`java` / `node` / …）。
    pub tool_id: String,
    /// 逻辑命令名（不含扩展名）。
    pub command: String,
    /// 文件名（含扩展名，`PATH` 解析按它找）。
    pub file: String,
    /// 第一个含这个文件的目录（按生效顺序）。`None` = `PATH` 上没有它。
    pub directory: Option<String>,
    /// 那个目录来自哪个作用域。
    pub scope: Option<&'static str>,
    /// 那个目录在生效顺序里的位置。
    pub index: Option<usize>,
    /// 是不是这个工具的主命令。
    pub primary: bool,
}

/// 第三方管理器的全局包前缀。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPrefix {
    /// 哪个工具的全局包（`node`）。
    pub tool_id: String,
    /// 前缀路径（原样，不规范化）。
    pub prefix: String,
    /// 这个前缀是怎么知道的。
    pub origin: PrefixOrigin,
    /// 前缀本身是不是 reparse point。
    pub inside_reparse: bool,
    /// 是哪种 reparse point。
    pub reparse_kind: Option<ReparseKind>,
    /// 它指向哪（symlink / junction 的目标）。
    pub link_target: Option<String>,
    /// 前缀（或它的目标）里那个版本号成分（`v24.19.0`）。有它就意味着
    /// **切版本会把这批全局包藏起来**。
    pub version_component: Option<String>,
}

/// 全局包前缀是从哪知道的。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrefixOrigin {
    /// 问了工具自己（`npm config get prefix`）。
    Probe,
    /// 从环境变量读到的（`NPM_CONFIG_PREFIX`）。
    EnvVar(String),
    /// 从版本管理器的"当前版本"链接变量推的（nvm4w 的 `NVM_SYMLINK`）。
    ///
    /// **这一档必须与上面两档分开**：它是推断，不是工具告诉我们的。
    ManagerLinkVar(String),
}

/// 一个"看起来是开发工具、却没有**机制**在管它"的目录。
///
/// # "没人管"有两个来源，两个都要收
///
/// 1. **只有文件系统扫描提到它**（`confidence = directory-only`）。这一档最实在：
///    扫描器能看见它，而注册表、`App Paths`、任何管理器都不知道它 ——
///    换一台机器**重建不出来**。本机的 `C:\Dev\Tool\apache-maven-3.9.5` 就是这一档。
/// 2. **一条记录都没有**：扫描根下面那些名字像工具、却谁也没提过的目录。
///
/// 第一版只做了第 2 种，于是在真机上报 0 条 —— 因为 `detect` 的文件系统扫描**就是**
/// 一种机制，它已经把 Maven 找到了。**"没有记录"与"只有扫描记录"是两件事**，
/// 而票据要报的是后者。
///
/// # "提到"与"管"是两件事，所以要分开记
///
/// `mentioned_by` 是**谁提过它**（含 `filesystem-scan` 与 `path-resolution`），
/// `managed_by` 是**谁在管它**（只有 `manager` 与 `tuoen` 算）。
/// 真机上 `C:\Dev\base\JDK` 两个都有值而 `managed_by` 为空：`PATH` 上确实有
/// `C:\Dev\base\JDK\JDK8\bin\java.exe`（所以它被提到），但没有任何管理器或注册表
/// 知道这个目录 —— **它仍然是"没人管"**。只用一个布尔量会把这两种情况混在一起
/// （第一版就是这么写的，于是真机上印出 `mentioned-by=filesystem-scan,path-resolution`
/// 与 `only-scan=true` 这种自相矛盾的证据）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevRoot {
    /// 目录本身。
    pub path: String,
    /// 为什么觉得它是开发工具。**是一个 ASCII slug，不是中文句子** ——
    /// 它会进 `evidence`，而 `--json` 的成功载荷必须能被脚本按字面匹配
    /// （中文解释在 `message` 里）。取值：`scan-only` / `name-matches-tool` /
    /// `contains-known-executable`。
    pub looks_like: &'static str,
    /// 它下面有多少个条目（分母）。
    pub children: usize,
    /// 哪些机制提到过它（`[]` = 谁都没提过）。
    pub mentioned_by: Vec<&'static str>,
    /// 哪些机制**在管**它（`manager` / `tuoen`）。空 = 没有任何机制管它。
    pub managed_by: Vec<&'static str>,
    /// 唯一提到它的是文件系统扫描（= 换一台机器重建不出来）。
    pub only_scan_mentions_it: bool,
}

/// 我们自己的 shim。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShimFacts {
    /// shim 目录（没有就 `None`）。
    pub dir: Option<String>,
    /// 那个目录在不在生效 `PATH` 上。
    pub on_path: bool,
    /// 盘上真实的 shim 命令名（不含扩展名），**不含模板**。
    pub commands: Vec<String>,
    /// 被别的目录抢在前面的命令（判据来自 `tuoen_platform::path::detect_shadowing`，
    /// 与 `path add` 的提醒**同源**）。空 vec 的含义必须先看 `commands`：
    /// 没有 shim 时它必然为空，那不是"没被遮蔽"，而是"没得比"。
    pub shadowed: Vec<ShadowFact>,
}

/// 一条被别的目录抢在前面的 shim 命令。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowFact {
    /// 命令名（不含扩展名），例如 `node`。
    pub command: String,
    /// 抢在我们前面的那个目录。
    pub by: String,
    /// 那个目录来自哪个作用域。
    pub by_scope: &'static str,
    /// 那个目录在生效顺序里的位置。
    pub by_index: usize,
    /// 那个目录里实际命中的文件（含扩展名），例如 `node.exe`。
    pub file: String,
}

/// 一个全局包前缀的探测方法。
struct PrefixProbe {
    tool_id: &'static str,
    /// 要跑的命令（`PATH` 上的文件名）。
    command: &'static str,
    args: &'static [&'static str],
    /// 退路：这些环境变量里的第一个有值的。
    env_vars: &'static [&'static str],
}

/// 只支持**真机上确有其事**的那一个（本机 `npm config get prefix` = `C:\nvm4w\nodejs`）。
///
/// 加一个工具就要有它的真机读数，否则那一条检查的"正例"会是编出来的。
const PREFIX_PROBES: &[PrefixProbe] = &[PrefixProbe {
    tool_id: "node",
    command: "npm.cmd",
    args: &["config", "get", "prefix"],
    env_vars: &["NPM_CONFIG_PREFIX", "PREFIX"],
}];

/// 一个根目录最多看几个孩子、最多报几条 —— 有上限才敢在真机上跑。
const MAX_SCAN_ROOTS: usize = 8;
const MAX_CHILDREN_PER_ROOT: usize = 60;

/// 采集全部事实。**只读**：没有一处写调用。
#[must_use]
pub fn collect_facts(ctx: &DetectContext<'_>, opts: &DoctorOptions) -> MachineFacts {
    // 时间戳只进 `captured_at` 字段，而 `doctor` 不写文件 —— 传空串，
    // 免得让"体检"看起来像"捕获"。
    let path = collect::collect_path(ctx, "", &opts.tuoen_roots);
    let (env, _skipped) = collect::collect_env(ctx, "");
    let tools = collect::collect_tools(ctx, "");
    let wsl = collect::collect_wsl(ctx, "");

    MachineFacts {
        resolution: collect_resolution(ctx, &path),
        global_prefix: collect_global_prefix(ctx, opts, &tools),
        dev_roots: collect_dev_roots(ctx, &tools),
        shims: collect_shims(ctx, opts, &path),
        system: collect_system(ctx),
        path,
        env,
        tools,
        wsl,
    }
}

/// 操作系统事实。
///
/// `Developer Mode` 与 `LongPathsEnabled` 的**值名是实测确认过的**，不是照抄文档：
/// 本机 `AllowDevelopmentWithoutDevLicense` 不存在（= 没开），
/// `LongPathsEnabled` 也不存在（= 没开）。票据专门提醒过不要照抄未经验证的路径。
#[must_use]
pub fn collect_system(ctx: &DetectContext<'_>) -> SystemFacts {
    SystemFacts {
        elevated: tuoen_platform::sys::is_elevated(),
        developer_mode: read_dword(
            ctx,
            RegHive::Hklm,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\AppModelUnlock",
            "AllowDevelopmentWithoutDevLicense",
        ),
        long_paths: read_dword(
            ctx,
            RegHive::Hklm,
            r"SYSTEM\CurrentControlSet\Control\FileSystem",
            "LongPathsEnabled",
        ),
    }
}

/// 读一个 DWORD 并翻成 `bool`。**键不在就是 `None`** —— 不是 `false`：
/// "这个开关不存在"与"这个开关关着"在报告里应当能被分开说。
fn read_dword(ctx: &DetectContext<'_>, hive: RegHive, subkey: &str, name: &str) -> Option<bool> {
    match ctx.registry.value(hive, subkey, name) {
        Some(RegValue::Dword(value)) => Some(value != 0),
        _ => None,
    }
}

/// 每条已知命令**解析到了哪个目录**（按生效顺序取第一个命中）。
///
/// 只在生效顺序上走一遍，**每个目录只列一次** —— 30 个目录 × 20 个命令名
/// 逐次 `inspect` 会是 600 次系统调用，而列目录一次就够。
#[must_use]
pub fn collect_resolution(ctx: &DetectContext<'_>, path: &PathFile) -> Vec<CommandResolution> {
    // 生效顺序 → 条目行 → （目录, 作用域, 位置）。
    let mut directories: Vec<(String, &'static str, usize)> = Vec::new();
    for reference in &path.effective {
        let Some(row) = path
            .entry
            .iter()
            .find(|row| row.scope == reference.scope && row.index == reference.index)
        else {
            continue;
        };
        if row.expanded.trim().is_empty() {
            continue;
        }
        directories.push((row.expanded.clone(), scope_slug(row.scope), row.index));
    }

    // 每个目录列一次。
    let listings: Vec<Vec<String>> = directories
        .iter()
        .map(|(dir, _, _)| {
            ctx.fs
                .list_dir(Path::new(dir))
                .into_iter()
                .map(|entry| entry.name)
                .collect()
        })
        .collect();

    let mut out = Vec::new();
    for tool in KNOWN_TOOLS {
        for name in tool.executables {
            let mut found = None;
            for (position, listing) in listings.iter().enumerate() {
                if listing.iter().any(|n| n.eq_ignore_ascii_case(name.file)) {
                    let (dir, scope, index) = &directories[position];
                    found = Some((dir.clone(), *scope, *index));
                    break;
                }
            }
            out.push(CommandResolution {
                tool_id: tool.id.to_owned(),
                command: name.command.to_owned(),
                file: name.file.to_owned(),
                directory: found.as_ref().map(|(dir, _, _)| dir.clone()),
                scope: found.as_ref().map(|(_, scope, _)| *scope),
                index: found.as_ref().map(|(_, _, index)| *index),
                primary: name.primary,
            });
        }
    }
    out
}

fn scope_slug(scope: EnvScope) -> &'static str {
    match scope {
        EnvScope::Machine => "machine",
        EnvScope::User => "user",
        EnvScope::ProcessOnly => "process-only",
    }
}

/// 第三方管理器的全局包前缀。
///
/// **只对真的被检测到的工具做**：没装 Node 的机器上"npm 的全局前缀在哪"
/// 是一个假问题，而假问题会让这条检查变成噪声源。
#[must_use]
pub fn collect_global_prefix(
    ctx: &DetectContext<'_>,
    opts: &DoctorOptions,
    tools: &ToolsFile,
) -> Vec<GlobalPrefix> {
    let mut out = Vec::new();
    for probe in PREFIX_PROBES {
        let detected = tools
            .tool
            .iter()
            .any(|row| row.name.eq_ignore_ascii_case(probe.tool_id));
        if !detected {
            continue;
        }
        let Some((prefix, origin)) = prefix_of(ctx, opts, probe) else {
            continue;
        };
        let facts = ctx.fs.inspect(Path::new(&prefix));
        let link_target = facts.link_target.clone();
        let version_component = version_component(&prefix)
            .or_else(|| link_target.as_deref().and_then(version_component));
        out.push(GlobalPrefix {
            tool_id: probe.tool_id.to_owned(),
            prefix,
            origin,
            inside_reparse: facts.reparse != ReparseKind::None,
            reparse_kind: (facts.reparse != ReparseKind::None).then_some(facts.reparse),
            link_target,
            version_component,
        });
    }
    out
}

fn prefix_of(
    ctx: &DetectContext<'_>,
    opts: &DoctorOptions,
    probe: &PrefixProbe,
) -> Option<(String, PrefixOrigin)> {
    // ① 问工具自己（最可信）。失败就往下走 —— 不编。
    if opts.probe_prefixes {
        let executable = ctx
            .process_env
            .path_entries()
            .into_iter()
            .map(|entry| Path::new(&entry).join(probe.command))
            .find(|candidate| ctx.fs.inspect(candidate).exists);
        if let Some(executable) = executable {
            let outcome = ctx.runner.run(&executable, probe.args, probe_timeout(opts));
            if !outcome.timed_out && outcome.spawned {
                let text = outcome.stdout.trim();
                if looks_like_path(text) {
                    return Some((text.to_owned(), PrefixOrigin::Probe));
                }
            }
        }
    }
    // ② 环境变量。
    for name in probe.env_vars {
        if let Some(value) = ctx.env_var(name).or_else(|| ctx.process_var(name))
            && looks_like_path(&value)
        {
            return Some((value, PrefixOrigin::EnvVar((*name).to_owned())));
        }
    }
    // ③ 版本管理器的"当前版本"链接变量 —— **推断**，所以单独一档。
    for manager in crate::detect::engine::KNOWN_MANAGERS {
        let Some(link_var) = manager.version_link_var else {
            continue;
        };
        if manager.id != probe.tool_id {
            continue;
        }
        if let Some(value) = ctx.env_var(link_var)
            && looks_like_path(&value)
        {
            return Some((value, PrefixOrigin::ManagerLinkVar(link_var.to_owned())));
        }
    }
    None
}

fn probe_timeout(opts: &DoctorOptions) -> Duration {
    opts.probe_timeout
}

/// 一个值像不像一条路径（用来挡住探测输出的垃圾行）。
fn looks_like_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

/// 路径里那个版本号成分（`v24.19.0` / `3.9.5`）。找到第一个就返回。
fn version_component(path: &str) -> Option<String> {
    path.split(['\\', '/'])
        .map(|part| part.trim_start_matches(['v', 'V']))
        .find(|part| {
            let mut parts = part.split('.');
            let first = parts.next().unwrap_or_default();
            !first.is_empty()
                && first.chars().all(|c| c.is_ascii_digit())
                && parts.clone().count() >= 1
                && parts.all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        })
        .map(str::to_owned)
}

/// "没有机制在管"的目录，两个来源都收（见 [`DevRoot`] 的说明）。
///
/// **它不是全盘扫描**：根目录来自 [`default_scan_roots`]（就是 `detect` 用的那一份），
/// 每个根只看一层，且有上限。
#[must_use]
pub fn collect_dev_roots(ctx: &DetectContext<'_>, tools: &ToolsFile) -> Vec<DevRoot> {
    let mut out = Vec::new();

    // 来源 ①：只有文件系统扫描提到它（`directory-only`）。
    //
    // 这一档是真机上唯一会响的一档：本机 `C:\Dev\Tool\apache-maven-3.9.5` 被扫描器
    // 找到了，而注册表、`PATH`、`App Paths`、任何管理器都不知道它。
    for row in tools
        .tool
        .iter()
        .filter(|row| row.confidence == "directory-only")
    {
        let path = Path::new(&row.path);
        let mentioned_by = mechanisms_for(tools, &row.path);
        out.push(DevRoot {
            path: row.path.clone(),
            looks_like: "scan-only",
            children: ctx.fs.list_dir(path).len(),
            // **"只有扫描提到"必须真的算出来**，不能因为这一档是从
            // `directory-only` 行来的就假定它（第一版就是这么写的，于是真机上印出
            // `mentioned-by=filesystem-scan,path-resolution` 与 `only-scan=true`
            // 这种自相矛盾的证据 —— 而矛盾的数据比没有数据更坏）。
            only_scan_mentions_it: mentioned_by == ["filesystem-scan"],
            managed_by: managing_mechanisms(&mentioned_by),
            mentioned_by,
        });
    }

    // 来源 ②：扫描根下面那些名字像工具、却谁也没提过的目录。
    for root in default_scan_roots(ctx.process_env)
        .into_iter()
        .take(MAX_SCAN_ROOTS)
    {
        let children = ctx.fs.list_dir(&root.path);
        for child in children
            .iter()
            .filter(|child| child.is_dir)
            .take(MAX_CHILDREN_PER_ROOT)
        {
            let path = root.path.join(&child.name);
            let path_text = path.to_string_lossy().into_owned();
            let Some(looks_like) = looks_like_dev_tool(ctx, &path, &child.name) else {
                continue;
            };
            if !mechanisms_for(tools, &path_text).is_empty() {
                continue;
            }
            let children_count = ctx.fs.list_dir(&path).len();
            out.push(DevRoot {
                path: path_text,
                looks_like,
                children: children_count,
                mentioned_by: Vec::new(),
                managed_by: Vec::new(),
                only_scan_mentions_it: false,
            });
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.dedup_by(|a, b| a.path == b.path);
    out
}

/// 哪些**机制**在管这个目录。
///
/// 只有 `manager` 与 `tuoen` 算"管"：`path-resolution` / `app-paths` / `registry-arp`
/// 说明"我们知道它"，但**没有任何东西负责把它装回来**；`filesystem-scan` 更是只看了一眼。
fn managing_mechanisms(mentioned_by: &[&'static str]) -> Vec<&'static str> {
    mentioned_by
        .iter()
        .copied()
        .filter(|mechanism| matches!(*mechanism, "manager" | "tuoen"))
        .collect()
}

/// 哪些**机制**提到过这个目录。
///
/// `filesystem-scan` 也在里面 —— 它是机制，只是最弱的那一个（换机器重建不出来）。
/// 判"没人管"时要看的是"除了扫描还有谁"，见 [`DevRoot::only_scan_mentions_it`]。
fn mechanisms_for(tools: &ToolsFile, path: &str) -> Vec<&'static str> {
    let needle = path.to_lowercase();
    let mut out: Vec<&'static str> = tools
        .tool
        .iter()
        .filter(|row| row.path.to_lowercase().starts_with(&needle))
        .map(|row| match row.source.as_str() {
            "tuoen" => "tuoen",
            "path-resolution" => "path-resolution",
            "app-paths" => "app-paths",
            "registry-arp" => "registry-arp",
            "manager" => "manager",
            _ => "filesystem-scan",
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

/// 这个目录为什么像开发工具：名字里有已知工具名，或者它下面有已知的可执行文件。
fn looks_like_dev_tool(ctx: &DetectContext<'_>, path: &Path, name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if KNOWN_TOOLS.iter().any(|tool| lower.contains(tool.id)) {
        return Some("name-matches-tool");
    }
    let known: Vec<&str> = spec::all_executable_files();
    let hit =
        ctx.fs.list_dir(path).into_iter().find(|entry| {
            !entry.is_dir && known.iter().any(|k| entry.name.eq_ignore_ascii_case(k))
        });
    hit.map(|_| "contains-known-executable")
}

/// 我们自己的 shim 目录、它上面的命令、它在不在 `PATH` 上、以及哪些命令被抢了。
///
/// "谁赢了名字冲突"**不在这里判** —— 它来自
/// [`tuoen_platform::path::detect_shadowing`]，与 `path add` 的提醒是同一个函数。
/// 抄一份到 core 里迟早会漂移（扩展名集合、每个目录只列一次、空目录的分母
/// 都是容易被抄错的地方），而漂移的后果是同一个仓库对同一台机器给出两个答案。
#[must_use]
pub fn collect_shims(ctx: &DetectContext<'_>, opts: &DoctorOptions, path: &PathFile) -> ShimFacts {
    let Some(dir) = opts.shim_dir.as_ref() else {
        return ShimFacts::default();
    };
    let dir_text = dir.to_string_lossy().into_owned();
    let on_path = path
        .entry
        .iter()
        .any(|row| same_dir(&row.expanded, &dir_text));

    let (shadowed, commands) = tuoen_platform::detect_shadowing(ctx.fs, &effective_refs(path), dir);

    // 模板不是命令。正常情况下它在 `tuoen.exe` 旁边、不在 shim 目录里，
    // 但"万一被放进来就当一条命令报出去"是一个很容易避免的假话。
    let mut commands: Vec<String> = commands
        .into_iter()
        .filter(|command| {
            !format!("{command}.exe").eq_ignore_ascii_case(tuoen_shim::TEMPLATE_FILE_NAME)
        })
        .collect();
    commands.sort();
    commands.dedup();

    ShimFacts {
        dir: Some(dir_text),
        on_path,
        commands,
        shadowed: shadowed
            .into_iter()
            .map(|shim| ShadowFact {
                command: shim.command,
                by: shim.by.value,
                by_scope: scope_slug(shim.by.scope),
                by_index: shim.by.index,
                file: shim.file,
            })
            .collect(),
    }
}

/// 生效顺序（逐条带出处），供平台层的遮蔽检测使用。
///
/// **只按 `[[effective]]` 走**：启动器注入项的位置推不出来（决策 87），
/// 而遮蔽检测要的正是"真实的先后"。
fn effective_refs(path: &PathFile) -> Vec<tuoen_platform::EntryRef> {
    path.effective
        .iter()
        .filter_map(|reference| {
            let row = path
                .entry
                .iter()
                .find(|row| row.scope == reference.scope && row.index == reference.index)?;
            Some(tuoen_platform::EntryRef {
                scope: row.scope,
                index: row.index,
                raw: row.raw.clone(),
                // `value` 是"用于比较的规范形态"。这里给它展开后的文本：
                // 遮蔽检测会拿它去拼 `<目录>\<命令>.<扩展名>`，而 `Path::join`
                // 对结尾反斜杠是宽容的，所以不需要再规范化一次。
                value: row.expanded.clone(),
            })
        })
        .collect()
}

/// 两个目录文本是不是同一个目录（忽略大小写与尾部反斜杠）。
fn same_dir(a: &str, b: &str) -> bool {
    a.trim()
        .trim_matches('"')
        .trim_end_matches('\\')
        .to_lowercase()
        == b.trim_end_matches('\\').to_lowercase()
}
