//! 把全局包装进 **tuoen 自己的根**：分类、命令形状、staging + 翻转、逐包结果（票据 #24）。
//!
//! # 这一层回答三个问题
//!
//! 1. **要装什么**（[`globals_wanted`]）—— 两个来源的**并集**，按 `(tool, name)` 去重；
//!    本机我们自己的根里已经有的那些不算。**纯函数**，不碰磁盘、不起进程。
//! 2. **缓存里有没有**（[`probe_npm_cache`]）—— 计划里的 `needsNetwork` 是**量出来的**，
//!    不是"有 install 动作就算要网络"（票据 #24 §3）。
//! 3. **真装**（[`install_globals`]）—— 逐包 staging + 翻转，一个失败不拖垮整节。
//!
//! # 幂等判据是**集合**，不是"目录在不在"（决策 203）
//!
//! `mise` 的模型是一个包一个版本目录，所以它的 `is_install_satisfied` 只看那个目录；
//! tuoen 的模型是**一个根装 N 个包**（`globals\npm\v24.19.0`），所以"装好了吗"只能比
//! **枚举出来的 `(包, 版本)` 集合** —— 抄 `mise` 那条判据就是"目录在 ⇒ 一致了"。
//!
//! 比的**只能是本机 tuoen 那一侧**：机器侧那个前缀（本机是 `C:\nvm4w\nodejs`）是
//! 第三方版本管理器的地盘（决策 154 永不写），拿它比会**永远** `would-change`。
//!
//! # 两个工具的不对称是**实测**出来的，不是猜的（决策 197/198/199）
//!
//! * npm：`npm install -g --prefix <根>` 只看 `<根>\node_modules`（机器侧已有同名同版本时
//!   照样装进我们的根）；
//! * pip：`pip install --user` 在**解析索引之前**就会因为"已经满足"短路 —— 退出码 0、
//!   输出一句完全合理的 `Requirement already satisfied`，而 tuoen 的根里**一个字节都没有**。
//!   所以 pip 那条命令**必须**带 `--ignore-installed`，否则幂等永远不收敛。
//! * 裸 `python` 在这台机器上是 0 字节的 App Execution Alias（决策 199）：pip 一律用 `pip.exe`。
//!
//! # 逐包 staging + 翻转（不是整节一把）
//!
//! 票据 #24 说"staging + 翻转"，而同一张票据又说"一个失败不拖垮整节、整节 outcome 说清
//! 7 个里成了 6 个"。两者同时成立只有一种做法：**每个包各自 staging、各自翻转** ——
//! 于是失败的包连一个字节都没进正式目录（"正式目录逐字节未变"），成功的包照旧落地。
//! 这是 L0 `install` 的先例（"每一行都是一次 install，而 install 自己就是原子的"）在
//! 这一节的形状。
//!
//! # 子进程输出里的凭据
//!
//! npm / pip 的 stdout+stderr 会进报告，所以**必须先过 [`secrets::scan_content`]**：
//! 命中的 token 换成 `[redacted:<shape>]`，形状记进 [`PackageOutcome::redactions`]。
//! 替换之后再复核一次；还不干净就把整段输出换成一个占位符（**宁可少说，也不漏材料**）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tuoen_platform::{ProcessOutcome, ProcessRunner};

use crate::capture::collect::Command;
use crate::capture::collect::globals::GLOBALS_TIMEOUT;
use crate::capture::files::{GlobalRow, GlobalsFile};
use crate::capture::secrets;
use crate::detect::DetectContext;
use crate::restore::manual::{ascii_layout, ascii_token};
use crate::restore::sections::same_version;

use super::{
    GlobalsRoot, GlobalsSource, GlobalsTool, NPM_PREFIX_VAR, PIP_USER_VALUE, PIP_USER_VAR,
    PYTHONUSERBASE_VAR, UNKNOWN_VERSION,
};

// ─────────────────────────────────────────────────────────────────────────────
// 契约常量
// ─────────────────────────────────────────────────────────────────────────────

/// 装一个全局包的超时（票据 #24 钉死的值）。
///
/// 与枚举那个 [`GLOBALS_TIMEOUT`]（60 s）**分开**：枚举只是问一句，安装要下载、
/// 要解包、要跑生命周期脚本。本机实测离线装一个 `pnpm@11.21.0` 是 **1755 ms**，
/// 但在线冷装一个带原生依赖的包可以是分钟级 —— 300 s 是"够用且不会挂死"的那条线。
pub const GLOBAL_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

/// staging 目录的名字前缀（与 `crates/archive` 的 `STAGING_PREFIX` 同一个形状）。
///
/// 它在**正式目录里面**（`<根>\.staging-<n>`），所以"同一个卷"这件事是结构保证的 ——
/// 翻转可以用 `rename` 而不是跨卷复制。
pub const STAGING_PREFIX: &str = ".staging-";

/// 逐包结果的稳定 slug（`--json` 里逐字出现，**不本地化**）。
pub const RESULT_INSTALLED: &str = "installed";
/// 本机我们自己的根里已经有这个 `(包, 版本)` —— 不重装（决策 155）。
pub const RESULT_ALREADY_PRESENT: &str = "already-present";
/// 缓存里没有，而这一趟不许上网（`--offline`）—— **一个字节都没写**。
pub const RESULT_NOT_CACHED: &str = "not-cached";
/// 那个版本在源上不存在（npm 的 `E404` / `ETARGET`），或者快照根本没记下版本。
pub const RESULT_VERSION_NOT_FOUND: &str = "version-not-found";
/// 安装命令失败了（非零退出 / 超时 / 没起来 / 装完产物不在）。
pub const RESULT_INSTALL_FAILED: &str = "install-failed";
/// 同一个包两个来源版本不同 —— 一个都不装（票据 #24 的安装期规则）。
pub const RESULT_VERSION_CONFLICT: &str = "version-conflict";
/// 本机这一侧答不上来（工具不在 `PATH`、运行时版本未知、算不出我们的根）。
pub const RESULT_UNSUPPORTED: &str = "unsupported";
/// **词表成员，本票不产出**：包的 bin 被更高优先级的同名命令遮住了（spec §4 的遮蔽语义）。
///
/// 发 shim 是 #25 的事，而"遮蔽"只有真的发了 shim 之后才谈得上 —— 所以 #24 的
/// 任何一条路径都不会产生这个 slug。把它写在这里是因为它是**结果词表的一部分**：
/// 少一个成员，消费者（GUI / 脚本）就会把不认识的 slug 当成"产品坏了"。
pub const RESULT_SKIPPED_SHADOWED: &str = "skipped-shadowed";

/// 失败的稳定 code（进 `apply.failures[]`，与结果 slug 同源）。
pub const FAILURE_NOT_CACHED: &str = "not-cached";
/// 见 [`RESULT_VERSION_NOT_FOUND`]。
pub const FAILURE_VERSION_NOT_FOUND: &str = "version-not-found";
/// 见 [`RESULT_INSTALL_FAILED`]。
pub const FAILURE_INSTALL_FAILED: &str = "install-failed";
/// 这一趟要网络，而用户说了 `--offline`（票据 #24 的安装期补充规则②）。
pub const FAILURE_NEEDS_NETWORK: &str = "needs-network";

// ─────────────────────────────────────────────────────────────────────────────
// 命令形状（**只有这一处**，调用点不拼字符串）
// ─────────────────────────────────────────────────────────────────────────────

/// npm 的安装：`npm install -g --prefix <staging> <name>@<version>`。
///
/// 形状与 `mise` 的 `npm.rs:808-819` **完全相同**（决策 203 的对照）；我们**多**做的一件事
/// 是同时把 `NPM_CONFIG_PREFIX` 塞进子进程环境（决策 190 的两条路：一条写错另一条还在）。
///
/// **刻意不加 `--ignore-scripts`**（决策 203）：tuoen 的场景是"复现一台机器"，
/// 一部分包不跑脚本就是坏的 —— 换来的是"安装成功、命令存在、一跑就炸"这种假话。
/// 代价是这条命令会执行来自包本身的代码，所以人类输出**必须说出来**。
const NPM_INSTALL: Command = Command {
    program: "cmd.exe",
    args: &["/C", "npm.cmd", "install", "-g", "--prefix"],
};

/// pip 的安装：`pip.exe install --user --ignore-installed --disable-pip-version-check <name>==<version>`。
///
/// `--ignore-installed` 是**必需**的（决策 197 的实测）：不带它时 pip 会因为"机器自己那份
/// 已经满足"而在解析索引之前短路 —— 退出码 0、输出看起来完全合理，而 tuoen 的根里
/// 一个字节都没有，于是第二次 `restore` 又"要装"，**幂等永远不收敛**。
const PIP_INSTALL: Command = Command {
    program: "pip.exe",
    args: &[
        "install",
        "--user",
        "--ignore-installed",
        "--disable-pip-version-check",
    ],
};

/// npm 的**缓存探针**：`npm pack --dry-run --offline <name>@<version>`。
///
/// 为什么是它：① `--dry-run` **不落一个字节**（实测：在空目录里跑完，目录还是空的）；
/// ② 它要的是**真 tarball**（比 `install --dry-run` 更强 —— 后者连包都没解）；
/// ③ 缓存未命中时它是**快速失败**（实测 `ENOTCACHED`、退出 1、~0.5 s）。
///
/// **不能**用 `npm cache ls` 判：它按 URL 列缓存条目，同一个包从别的源（npmmirror）
/// 缓存过时它照样"有"，而当前 registry 是 `registry.npmjs.org` —— 判据会假绿。
const NPM_CACHE_PROBE: Command = Command {
    program: "cmd.exe",
    args: &["/C", "npm.cmd", "pack", "--dry-run", "--offline"],
};

// ─────────────────────────────────────────────────────────────────────────────
// 分类（纯函数）
// ─────────────────────────────────────────────────────────────────────────────

/// 一个要装进我们根里的包。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalInstall {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 包名（含 scope）。
    pub name: String,
    /// 快照里钉死的**精确**版本（`pkg@<version>` 的那个版本）。
    pub version: String,
    /// 装进哪里：npm → `<base>\npm\<本机 node -v>`，pip → `<base>\pip`。
    pub target: PathBuf,
    /// 这个包在快照里的来源（`machine` / `tuoen`，可能两个都有 —— 版本相同时装一次）。
    pub sources: Vec<&'static str>,
    /// 这一趟**要不要网络**：缓存里能解决就是 `false`（票据 #24 §3）。
    pub needs_network: bool,
}

/// 本机我们自己的根里已经有这个 `(包, 版本)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPresent {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 包名。
    pub name: String,
    /// 版本（本机根里那个）。
    pub version: String,
    /// 快照里的来源。
    pub sources: Vec<&'static str>,
}

/// 同一个 `(tool, name)` 在两个来源里版本不同 —— **一个都不装**。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalConflict {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 包名。
    pub name: String,
    /// 两个来源各自的版本，**顺序固定**（machine 在前）：`[("machine", "11.21.0"), …]`。
    pub versions: Vec<(&'static str, String)>,
}

/// 这一票做不到的那一个包（工具不在本机 `PATH` 上 / 运行时版本答不出来 / 算不出我们的根）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalUnsupported {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 包名。
    pub name: String,
    /// 快照里的版本（可能是 `unknown`）。
    pub version: String,
    /// 具体原因的稳定 slug：`tool-not-on-path` / `runtime-version-unknown` /
    /// `globals-root-unknown` / `version-unknown`。
    pub reason: &'static str,
}

/// 快照里那个前缀不是我们装进去的地方（票据 #24 §4 的**必需部分**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalPrefixMoved {
    /// 哪个工具。
    pub tool: GlobalsTool,
    /// 快照里那些包**实际在**的地方（`C:\nvm4w\nodejs`）。
    pub from: String,
    /// tuoen 装进的地方（`<base>\npm\v24.19.0`）。
    pub to: PathBuf,
}

/// "要装什么"的全部结论（纯函数）。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GlobalsWanted {
    /// 要装的（按 `(tool, name)` 排序，顺序稳定）。
    pub installs: Vec<GlobalInstall>,
    /// 本机根里已经有的。
    pub already_present: Vec<GlobalPresent>,
    /// 两个来源版本不同的。
    pub conflicts: Vec<GlobalConflict>,
    /// 做不到的。
    pub unsupported: Vec<GlobalUnsupported>,
    /// 本机根里**多出来**的 `(tool, name)`（我们**从不卸载**，只报数）。
    pub extra: Vec<String>,
    /// 本机我们自己的根**读不出来**（枚举失败）—— "不知道"绝不许被当成"没有"。
    pub root_unreadable: bool,
    /// 前缀搬家那件事（每个工具最多一条）。
    pub prefix_moved: Vec<GlobalPrefixMoved>,
}

impl GlobalsWanted {
    /// 这一节要装几个包。
    #[must_use]
    pub fn install_count(&self) -> usize {
        self.installs.len()
    }

    /// 这一趟有没有**必须联网**的包。
    #[must_use]
    pub fn needs_network(&self) -> bool {
        self.installs.iter().any(|install| install.needs_network)
    }

    /// 必须联网的那些包（`name@version`，顺序稳定）—— 拒绝 `--offline` 时要点名它们。
    #[must_use]
    pub fn network_packages(&self) -> Vec<String> {
        self.installs
            .iter()
            .filter(|install| install.needs_network)
            .map(|install| format!("{}@{}", install.name, install.version))
            .collect()
    }
}

/// 缓存探针的键：`<tool>:<name>@<version>`。
///
/// **它就是那个三态**：键不在 = 没探过（不是"没有缓存"），`false` = 探过、缓存里没有，
/// `true` = 探过、缓存里能解决。
#[must_use]
pub fn cache_key(tool: GlobalsTool, name: &str, version: &str) -> String {
    format!("{}:{name}@{version}", tool.slug())
}

/// 两个来源的并集，减去本机我们自己的根里已经有的那些。**纯函数**。
///
/// # 参数
///
/// * `target` —— 快照（目标侧）；
/// * `local` —— 本机现场（可能是 `None`：那一侧没采 `globals`）；
/// * `base` —— **我们自己的根**（`%LOCALAPPDATA%\tuoen\globals`）。它是**传进来的**，
///   不是在这里读进程环境算的 —— `plan` 必须是纯函数，而"根在哪"是 CLI 的事；
/// * `cache` —— 探针结果（[`probe_npm_cache`]）。空表 = 什么都没探到 ⇒ 一律算"要网络"
///   （**不许**把"没探过"当成"缓存里有"）。
///
/// # 判据顺序（固定，先命中先算）
///
/// ① 两个来源版本不同 → 冲突（一个都不装）；② 版本答不出来 → 做不到；
/// ③ 本机这一侧答不上来（工具不在 `PATH` / 运行时版本未知 / 没有根）→ 做不到；
/// ④ 本机根里已有同名同版本 → `already-present`；⑤ 其余 → 装。
#[must_use]
pub fn globals_wanted(
    target: &GlobalsFile,
    local: Option<&GlobalsFile>,
    base: Option<&Path>,
    cache: &BTreeMap<String, bool>,
) -> GlobalsWanted {
    let mut wanted = GlobalsWanted::default();

    for tool in GlobalsTool::ALL {
        // ── 目标侧：两个来源的包，按名字合起来 ──────────────────────────────
        let mut by_name: BTreeMap<String, Vec<(&'static str, String)>> = BTreeMap::new();
        let mut machine_prefix: Option<String> = None;
        for row in target.global.iter().filter(|row| row.tool == tool.slug()) {
            let Some(source) = GlobalsSource::from_slug(&row.source) else {
                // 不认识的 `source`：**不猜**（第三个数不是我们的）。它不属于任何一侧，
                // 于是它既不会被装、也不会被算成"本机已有"。
                continue;
            };
            if source == GlobalsSource::Machine && machine_prefix.is_none() {
                machine_prefix = row.prefix.clone();
            }
            for package in &row.packages {
                let entry = by_name.entry(package.name.clone()).or_default();
                if !entry.iter().any(|(known, _)| *known == source.slug()) {
                    entry.push((source.slug(), package.version.clone()));
                }
            }
        }

        // ── 本机侧：我们自己的根里有什么 ────────────────────────────────────
        let local_row = local.and_then(|file| tuoen_row(file, tool));
        let local_machine = local.and_then(|file| machine_row(file, tool));
        // **读不出来**（枚举失败）与"根是空的"必须分得开（决策 175 的三态）：
        // 前者当"全缺"去装（宁可多装一遍，也不要把"不知道"写成"没有"）。
        let root_unreadable = local_row.is_some_and(|row| row.enumerate_error.is_some());
        wanted.root_unreadable |= root_unreadable;
        let local_packages: Vec<(&str, &str)> = if root_unreadable {
            Vec::new()
        } else {
            local_row
                .map(|row| {
                    row.packages
                        .iter()
                        .map(|package| (package.name.as_str(), package.version.as_str()))
                        .collect()
                })
                .unwrap_or_default()
        };

        // ── 我们装进哪里 ────────────────────────────────────────────────────
        // 根名用**本机**运行时的原样版本（决策 194 的接缝），不是快照里那个：
        // 装到别的版本的目录里，本机的 shell 永远看不到它们。
        let local_version = local_machine.map(|row| row.tool_version.as_str());
        let target_dir = match (tool, base) {
            (_, None) => None,
            (GlobalsTool::Npm, Some(base)) => local_version
                .and_then(|version| GlobalsRoot::from_base(base.to_path_buf()).npm_prefix(version)),
            (GlobalsTool::Pip, Some(base)) => {
                Some(GlobalsRoot::from_base(base.to_path_buf()).pip_userbase())
            }
        };
        // 工具不在本机 `PATH` 上（连一行都没有）时**不装**：装进去也没有任何东西会用它。
        let tool_here = local_machine.is_some();
        let version_unknown = local_version == Some(UNKNOWN_VERSION) || local_version.is_none();

        // ── 前缀搬家：快照里那个前缀不是我们装进去的地方 ────────────────────
        // 判据要**先有地方可去**（根算得出来、而且这个工具真的在本机），否则会印一句
        // "我们装到这儿"而那儿不会有任何东西。
        if tool_here
            && let (Some(from), Some(to)) = (machine_prefix.as_deref(), target_dir.as_ref())
            && !same_path(from, to)
        {
            wanted.prefix_moved.push(GlobalPrefixMoved {
                tool,
                from: from.to_owned(),
                to: to.clone(),
            });
        }

        // ── 逐个包判 ────────────────────────────────────────────────────────
        for (name, versions) in by_name {
            let sources: Vec<&'static str> = versions.iter().map(|(source, _)| *source).collect();

            // ① 两个来源版本不同 → 冲突（**绝不静默挑一个**）。
            let agrees = versions
                .iter()
                .all(|(_, left)| versions.iter().all(|(_, right)| same_version(left, right)));
            if versions.len() > 1 && !agrees {
                wanted.conflicts.push(GlobalConflict {
                    tool,
                    name,
                    versions,
                });
                continue;
            }
            let version = versions
                .first()
                .map(|(_, version)| version.clone())
                .unwrap_or_else(|| UNKNOWN_VERSION.to_owned());

            // ② 版本答不出来 → 做不到（`pkg@unknown` 是一句没有内容的话）。
            if version.is_empty() || version == UNKNOWN_VERSION {
                wanted.unsupported.push(GlobalUnsupported {
                    tool,
                    name,
                    version,
                    reason: "version-unknown",
                });
                continue;
            }
            // ③ 本机这一侧答不上来（工具不在 PATH / 运行时版本未知 / 算不出我们的根）。
            let reason = if !tool_here {
                Some("tool-not-on-path")
            } else if base.is_none() {
                Some("globals-root-unknown")
            } else if version_unknown && tool == GlobalsTool::Npm {
                Some("runtime-version-unknown")
            } else {
                None
            };
            let Some(target_dir) = target_dir.as_ref().filter(|_| reason.is_none()) else {
                wanted.unsupported.push(GlobalUnsupported {
                    tool,
                    name,
                    version,
                    reason: reason.unwrap_or("globals-root-unknown"),
                });
                continue;
            };

            // ④ 本机根里已有同名同版本 → 不重装。
            if local_packages
                .iter()
                .any(|(known, mine)| *known == name && same_version(&version, mine))
            {
                wanted.already_present.push(GlobalPresent {
                    tool,
                    name,
                    version,
                    sources,
                });
                continue;
            }

            // ⑤ 装。
            let needs_network = match tool {
                // npm：**量出来的**（探针）。pip：没有可证明的离线解析路径 ⇒ 一律要网络。
                GlobalsTool::Npm => cache.get(&cache_key(tool, &name, &version)) != Some(&true),
                GlobalsTool::Pip => true,
            };
            wanted.installs.push(GlobalInstall {
                tool,
                name,
                version,
                target: target_dir.clone(),
                sources,
                needs_network,
            });
        }

        // ── 本机根里多出来的：只报数，从不卸载 ──────────────────────────────
        let wanted_names: Vec<&str> = wanted
            .installs
            .iter()
            .filter(|install| install.tool == tool)
            .map(|install| install.name.as_str())
            .chain(
                wanted
                    .already_present
                    .iter()
                    .filter(|present| present.tool == tool)
                    .map(|present| present.name.as_str()),
            )
            .collect();
        for (name, _) in &local_packages {
            if !wanted_names.contains(name) {
                wanted.extra.push(format!("{}:{name}", tool.slug()));
            }
        }
    }

    // 顺序稳定（两次调用逐条相同）：按 `(tool, name)`。
    wanted
        .installs
        .sort_by(|left, right| (left.tool, &left.name).cmp(&(right.tool, &right.name)));
    wanted
        .already_present
        .sort_by(|left, right| (left.tool, &left.name).cmp(&(right.tool, &right.name)));
    wanted
        .conflicts
        .sort_by(|left, right| (left.tool, &left.name).cmp(&(right.tool, &right.name)));
    wanted
        .unsupported
        .sort_by(|left, right| (left.tool, &left.name).cmp(&(right.tool, &right.name)));
    wanted.extra.sort();
    wanted
}

/// 一个工具在**本机现场**里 `source = "tuoen"` 的那一行。
fn tuoen_row(file: &GlobalsFile, tool: GlobalsTool) -> Option<&GlobalRow> {
    file.global
        .iter()
        .find(|row| row.tool == tool.slug() && row.source == GlobalsSource::Tuoen.slug())
}

/// 一个工具在**本机现场**里 `source = "machine"` 的那一行（本机有没有这个工具、版本是什么）。
fn machine_row(file: &GlobalsFile, tool: GlobalsTool) -> Option<&GlobalRow> {
    file.global
        .iter()
        .find(|row| row.tool == tool.slug() && row.source == GlobalsSource::Machine.slug())
}

/// 两条路径说的是不是同一个地方（大小写不敏感、忽略尾部分隔符）。
///
/// **不做规范化**（不展开 `..`、不解析 reparse point）：这里判的是"快照里那个前缀
/// 是不是我们装进去的地方"，而两条路径都是原样字符串 —— 展开会造出**看起来更对**
/// 的结论（决策 173 的教训：判据越"聪明"，假绿的机会越多）。
fn same_path(left: &str, right: &Path) -> bool {
    let right = right.to_string_lossy();
    let trim = |text: &str| {
        text.trim_end_matches(['\\', '/'])
            .trim()
            .to_ascii_lowercase()
    };
    !left.is_empty() && trim(left) == trim(&right)
}

// ─────────────────────────────────────────────────────────────────────────────
// 缓存探针（会起进程，但**绝不上网**）
// ─────────────────────────────────────────────────────────────────────────────

/// 逐个问 npm："这个包在缓存里吗"。
///
/// 结果直接进 [`globals_wanted`] 的 `cache` 参数 —— 于是计划里的 `needsNetwork`
/// 是**量出来的**。npm 不在 `PATH` 上时**一个进程都不起**（空表 ⇒ 一律算要网络）。
#[must_use]
pub fn probe_npm_cache(
    ctx: &DetectContext<'_>,
    installs: &[GlobalInstall],
) -> BTreeMap<String, bool> {
    let mut facts = BTreeMap::new();
    let npm = installs
        .iter()
        .filter(|install| install.tool == GlobalsTool::Npm)
        .collect::<Vec<_>>();
    if npm.is_empty() || crate::capture::collect::find_on_path(ctx, "npm.cmd").is_none() {
        return facts;
    }
    for install in npm {
        let spec = format!("{}@{}", install.name, install.version);
        let outcome =
            NPM_CACHE_PROBE.run_with(ctx, std::slice::from_ref(&spec), &[], GLOBALS_TIMEOUT);
        facts.insert(
            cache_key(install.tool, &install.name, &install.version),
            probe_says_cached(&outcome),
        );
    }
    facts
}

/// 探针的判据：**真的成功**才算"缓存里有"（没起来 / 超时 / 非零退出都算没有）。
fn probe_says_cached(outcome: &ProcessOutcome) -> bool {
    outcome.spawned && !outcome.timed_out && outcome.exit_code == Some(0)
}

// ─────────────────────────────────────────────────────────────────────────────
// 逐包结果（`--json` 的形状）
// ─────────────────────────────────────────────────────────────────────────────

/// 一个来源的版本（两个来源版本冲突时才有）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageVersion {
    /// `machine` / `tuoen`。
    pub source: &'static str,
    /// 那个来源里的版本。
    pub version: String,
}

/// 一个包的结局（`apply.sections[globals].packages[]` 的一行）。
///
/// 形状来自票据 #24 的 Implementation Decisions：`{tool, name, result}` + 版本那一格。
/// **`version` 与 `versions` 不会同时出现**：两个来源一致时是前者（装一次），
/// 不一致时是后者（一个都不装，把两个版本都摆出来）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageOutcome {
    /// 工具 slug。
    pub tool: &'static str,
    /// 包名（纯 ASCII）。
    pub name: String,
    /// 结果 slug（见本模块顶部的词表）。
    pub result: &'static str,
    /// 装（或已有）的版本 —— 两个来源一致时。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// 快照里的来源。
    pub sources: Vec<&'static str>,
    /// 两个来源各自的版本（冲突时）。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub versions: Vec<PackageVersion>,
    /// 稳定补充数据（`code=ENOTCACHED exit=1` / `reason=version-unknown`），**纯 ASCII**。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// 子进程的输出（**已经脱敏、已经压成 ASCII、已经截断**）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    /// 脱敏时命中过的形状 slug。
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub redactions: Vec<&'static str>,
}

/// 一处没做成（进 `apply.failures[]`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalsFailure {
    /// 稳定 code。
    pub code: &'static str,
    /// 稳定补充数据（**纯 ASCII**）。
    pub detail: String,
}

/// 一次安装的全部结果。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GlobalsInstallReport {
    /// 逐包结局（顺序与 [`GlobalsWanted`] 一致：已有/冲突/做不到的在前，要装的在后）。
    pub packages: Vec<PackageOutcome>,
    /// 有没有任何一个包真的落了盘。
    pub wrote: bool,
    /// 没做成的那些（**不含** `version-conflict` / `unsupported`：那是计划里的待办，
    /// 不是这一趟的失败）。
    pub failures: Vec<GlobalsFailure>,
}

impl GlobalsInstallReport {
    /// 有几个包真的装上了。
    #[must_use]
    pub fn installed_count(&self) -> usize {
        self.packages
            .iter()
            .filter(|package| package.result == RESULT_INSTALLED)
            .count()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 真装
// ─────────────────────────────────────────────────────────────────────────────

/// 把 [`GlobalsWanted`] 里要装的包装进我们自己的根。**逐包 staging + 翻转**。
///
/// `offline` 为真时，任何 `needs_network` 的包**一个字节都不写**（结果是 `not-cached`）——
/// `--offline` 是"绝不碰网络"的承诺，不是"优先用缓存"。
#[must_use]
pub fn install_globals(
    runner: &dyn ProcessRunner,
    wanted: &GlobalsWanted,
    offline: bool,
) -> GlobalsInstallReport {
    let mut report = GlobalsInstallReport::default();

    // 已经有的、冲突的、做不到的：**不跑任何命令**，但逐条出现在报告里
    // （`already-present` 与 `version-conflict` 都必须可达 —— 消费者要能对上计划里的每一条）。
    for present in &wanted.already_present {
        report.packages.push(PackageOutcome {
            tool: present.tool.slug(),
            name: ascii_token(&present.name),
            result: RESULT_ALREADY_PRESENT,
            version: Some(present.version.clone()),
            sources: present.sources.clone(),
            versions: Vec::new(),
            detail: None,
            output: None,
            redactions: Vec::new(),
        });
    }
    for conflict in &wanted.conflicts {
        report.packages.push(PackageOutcome {
            tool: conflict.tool.slug(),
            name: ascii_token(&conflict.name),
            result: RESULT_VERSION_CONFLICT,
            version: None,
            sources: conflict
                .versions
                .iter()
                .map(|(source, _)| *source)
                .collect(),
            versions: conflict
                .versions
                .iter()
                .map(|(source, version)| PackageVersion {
                    source,
                    version: version.clone(),
                })
                .collect(),
            detail: None,
            output: None,
            redactions: Vec::new(),
        });
    }
    for unsupported in &wanted.unsupported {
        report.packages.push(PackageOutcome {
            tool: unsupported.tool.slug(),
            name: ascii_token(&unsupported.name),
            result: RESULT_UNSUPPORTED,
            version: Some(unsupported.version.clone()),
            sources: Vec::new(),
            versions: Vec::new(),
            detail: Some(format!("reason={}", unsupported.reason)),
            output: None,
            redactions: Vec::new(),
        });
    }

    for install in &wanted.installs {
        // `--offline` 的承诺：缓存里没有就**一个字节都不写**。
        if offline && install.needs_network {
            report.packages.push(PackageOutcome {
                tool: install.tool.slug(),
                name: ascii_token(&install.name),
                result: RESULT_NOT_CACHED,
                version: Some(install.version.clone()),
                sources: install.sources.clone(),
                versions: Vec::new(),
                detail: Some("offline=yes cached=no".to_owned()),
                output: None,
                redactions: Vec::new(),
            });
            report.failures.push(GlobalsFailure {
                code: FAILURE_NOT_CACHED,
                detail: format!("{}@{}", install.name, install.version),
            });
            continue;
        }

        let outcome = install_one(runner, install, offline);
        if outcome.result == RESULT_INSTALLED {
            report.wrote = true;
        } else if outcome.result != RESULT_ALREADY_PRESENT {
            report.failures.push(GlobalsFailure {
                code: failure_code(outcome.result),
                detail: ascii_layout(
                    format!(
                        "{}@{} {}",
                        install.name,
                        install.version,
                        outcome.detail.as_deref().unwrap_or("")
                    )
                    .trim_end(),
                ),
            });
        }
        report.packages.push(outcome);
    }

    report
}

/// 结果 slug → 失败 code（两者是同一套词表）。
fn failure_code(result: &'static str) -> &'static str {
    match result {
        RESULT_NOT_CACHED => FAILURE_NOT_CACHED,
        RESULT_VERSION_NOT_FOUND => FAILURE_VERSION_NOT_FOUND,
        _ => FAILURE_INSTALL_FAILED,
    }
}

/// 装**一个**包：自己的 staging、自己的翻转。
///
/// 失败路径上要保证两件事（票据 #24 §1）：**正式目录逐字节未变**、**没有 `.staging-` 残余**。
/// 所以失败时删掉 staging，并且把我们**自己刚建出来**的那个正式目录也删掉（如果它是空的）——
/// "凭空多出一个空目录"也是改动。
fn install_one(
    runner: &dyn ProcessRunner,
    install: &GlobalInstall,
    offline: bool,
) -> PackageOutcome {
    let mut outcome = PackageOutcome {
        tool: install.tool.slug(),
        name: ascii_token(&install.name),
        result: RESULT_INSTALL_FAILED,
        version: Some(install.version.clone()),
        sources: install.sources.clone(),
        versions: Vec::new(),
        detail: None,
        output: None,
        redactions: Vec::new(),
    };

    let target = install.target.as_path();
    let created_target = !target.exists();
    if let Err(error) = std::fs::create_dir_all(target) {
        outcome.detail = Some(format!("reason=mkdir io={}", error.kind() as i32));
        return outcome;
    }
    // 上一趟崩了留下的 staging（同一个卷、同一个目录，`rename` 之外不可能有别人写）。
    clear_staging(target);

    let staging = match fresh_staging(target) {
        Ok(staging) => staging,
        Err(detail) => {
            outcome.detail = Some(detail);
            cleanup_empty_target(target, created_target);
            return outcome;
        }
    };

    let env = staging_env(install.tool, &staging);
    let args = install_args(install, &staging, offline);
    let (program, argv) = install.command_for(&args);
    let result = runner.run_env(Path::new(program), &argv, &env, GLOBAL_INSTALL_TIMEOUT);

    let (text, redactions) = redact_output(&result.combined());
    if !redactions.is_empty() {
        outcome.redactions = redactions;
    }
    if !text.trim().is_empty() {
        outcome.output = Some(report_text(&text));
    }

    match verdict_of(&result, install, offline) {
        Verdict::Installed => {
            if !artifact_present(install, &staging) {
                // **"命令成功"与"东西装上了"是两件事**（决策 197 的教训）：pip 会在
                // "已经满足"时退出 0 而一个字节都不写，所以产物必须自己看一眼。
                outcome.result = RESULT_INSTALL_FAILED;
                outcome.detail = Some("reason=no-artifact".to_owned());
                let _ = std::fs::remove_dir_all(&staging);
                cleanup_empty_target(target, created_target);
                return outcome;
            }
            match flip(&staging, target, install) {
                Ok(()) => {
                    outcome.result = RESULT_INSTALLED;
                    outcome.detail = Some(ascii_layout(&format!("to={}", target.display())));
                }
                Err(error) => {
                    outcome.result = RESULT_INSTALL_FAILED;
                    outcome.detail = Some(format!("reason=flip io={}", error.kind() as i32));
                    let _ = std::fs::remove_dir_all(&staging);
                    cleanup_empty_target(target, created_target);
                    return outcome;
                }
            }
        }
        Verdict::Failed(detail) => {
            outcome.result = RESULT_INSTALL_FAILED;
            outcome.detail = Some(detail);
        }
        Verdict::NotCached => {
            outcome.result = RESULT_NOT_CACHED;
            outcome.detail = Some("cached=no".to_owned());
        }
        Verdict::VersionNotFound => {
            outcome.result = RESULT_VERSION_NOT_FOUND;
            outcome.detail = Some("reason=version-not-found".to_owned());
        }
    }
    let _ = std::fs::remove_dir_all(&staging);
    cleanup_empty_target(target, created_target);
    outcome
}

/// 一条安装命令的判据（**只看机器 token，不看本地化的散文**）。
enum Verdict {
    /// 退出 0（产物另判）。
    Installed,
    /// 缓存里没有（`--offline` 之下）。
    NotCached,
    /// 那个版本在源上不存在。
    VersionNotFound,
    /// 其余失败（含超时、没起来）。
    Failed(String),
}

/// 退出码 + **机器 token** → 判据。
///
/// npm 的失败在 stderr 上带一行 `npm error code <CODE>`：那是**机器 token**，
/// 与界面语言无关（决策：永不匹配错误文本）。本机实测的两种形态：
/// `ENOTCACHED`（`--offline` 且缓存未命中，退出 1、~460 ms 快速失败）、
/// `E404` / `ETARGET`（版本不存在）。
fn verdict_of(outcome: &ProcessOutcome, install: &GlobalInstall, offline: bool) -> Verdict {
    if !outcome.spawned {
        return Verdict::Failed("reason=not-spawned".to_owned());
    }
    if outcome.timed_out {
        // **超时优先于退出码**：被超时杀掉的进程会带一个非零退出码，
        // 把它报成"安装失败"会指错方向（决策 175 的同一条顺序）。
        return Verdict::Failed("reason=timed-out".to_owned());
    }
    let code = npm_error_code(&outcome.combined());
    match outcome.exit_code {
        Some(0) => Verdict::Installed,
        Some(exit) => {
            let token = code.clone().unwrap_or_else(|| "none".to_owned());
            match code.as_deref() {
                Some("ENOTCACHED") => Verdict::NotCached,
                Some("E404" | "ETARGET") => Verdict::VersionNotFound,
                // pip 的 `--no-index` 之下，失败只可能是"本地没有那个 wheel"：
                // pip 没有一个稳定的机器 token（实测输出是一句英文散文），所以判据只能
                // 按**模式**给：离线 + 非零退出 ⇒ 缓存里没有。
                _ if install.tool == GlobalsTool::Pip && offline => Verdict::NotCached,
                _ => Verdict::Failed(format!("code={token} exit={exit}")),
            }
        }
        None => Verdict::Failed(format!(
            "code={} exit=none",
            code.unwrap_or_else(|| "none".to_owned())
        )),
    }
}

/// 从输出里取 npm 的机器 token：`npm error code <CODE>`。
///
/// 取的是**最后一条**：npm 在把根因包成一个更笼统的码时，具体的那一条在前 —— 但
/// `ENOTCACHED` 这种**具体**的码在整段输出里只出现一次，所以"最后一条"是安全的。
fn npm_error_code(text: &str) -> Option<String> {
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("npm error code ") {
            let code = rest.trim();
            if !code.is_empty() && code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                found = Some(code.to_owned());
            }
        }
    }
    found
}

impl GlobalInstall {
    /// 一条命令的形状：`(program, args)`。
    ///
    /// 参数拼在**结尾**，且只由这一处拼：`--prefix <staging> <name>@<version>` 里的三段
    /// 都是运行时值（staging 路径含用户目录、版本来自快照），常量表里放不下。
    /// 这里**不碰 `cmd.exe` 的载荷**：走的是普通 `arg`，与 `cmd.exe /C` 的 `raw_arg`
    /// 是两条路（决策 186 的那条界线）。
    fn command_for<'a>(&self, args: &'a [String]) -> (&'static str, Vec<&'a str>) {
        let command = match self.tool {
            GlobalsTool::Npm => NPM_INSTALL,
            GlobalsTool::Pip => PIP_INSTALL,
        };
        let mut argv: Vec<&str> = command.args.to_vec();
        argv.extend(args.iter().map(String::as_str));
        (command.program, argv)
    }
}

/// 一条安装命令的参数（`--offline` 只在用户说了算的时候加）。
fn install_args(install: &GlobalInstall, staging: &Path, offline: bool) -> Vec<String> {
    let staging = staging.to_string_lossy().into_owned();
    let spec = match install.tool {
        // npm 的 spec 就是 `name@version`（scope 里的 `@` 不影响）。
        GlobalsTool::Npm => format!("{}@{}", install.name, install.version),
        // pip 的 spec 是 `name==version`。
        GlobalsTool::Pip => format!("{}=={}", install.name, install.version),
    };
    match install.tool {
        GlobalsTool::Npm => {
            let mut args = vec![staging, spec];
            if offline {
                // 常量表里放不下这个开关（它随用户的选择变），所以它是**最后一段值**。
                args.push("--offline".to_owned());
            }
            args
        }
        GlobalsTool::Pip => {
            let mut args = vec![spec];
            if offline {
                args.push("--no-index".to_owned());
            }
            args
        }
    }
}

/// 子进程的重定向环境：**只走进程环境，绝不写 `.npmrc` / `pip.ini` / 注册表**（决策 27）。
///
/// 这里指向的是 **staging**（不是正式目录）：装进 staging、装完再翻 —— 于是
/// "装到一半崩了"的表现是"正式目录逐字节未变"，而不是"根里多了半个包"。
fn staging_env(tool: GlobalsTool, staging: &Path) -> Vec<(String, String)> {
    let value = staging.to_string_lossy().into_owned();
    match tool {
        GlobalsTool::Npm => vec![(NPM_PREFIX_VAR.to_owned(), value)],
        GlobalsTool::Pip => vec![
            (PYTHONUSERBASE_VAR.to_owned(), value),
            (PIP_USER_VAR.to_owned(), PIP_USER_VALUE.to_owned()),
        ],
    }
}

/// 清掉正式目录里遗留的 `.staging-*`（上一趟崩了留下的）。
///
/// 只删**这个名字前缀**的条目，而且只在**我们自己的**版本目录里 —— 那个目录是我们建的、
/// 里面只有我们装的东西（决策 26 的隔离）。删不掉不算失败：它不挡这一趟。
fn clear_staging(target: &Path) {
    let Ok(entries) = std::fs::read_dir(target) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
        {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// 找一个没被占用的 staging 目录并建出来。
fn fresh_staging(target: &Path) -> Result<PathBuf, String> {
    for index in 1..=u32::from(u16::MAX) {
        let candidate = target.join(format!("{STAGING_PREFIX}{index}"));
        if candidate.exists() {
            continue;
        }
        return match std::fs::create_dir_all(&candidate) {
            Ok(()) => Ok(candidate),
            Err(error) => Err(format!("reason=mkdir-staging io={}", error.kind() as i32)),
        };
    }
    Err("reason=staging-exhausted".to_owned())
}

/// 我们刚建出来的那个正式目录，如果还是空的就删掉（"凭空多出一个空目录"也是改动）。
fn cleanup_empty_target(target: &Path, created: bool) {
    if !created {
        return;
    }
    let empty = std::fs::read_dir(target)
        .map(|mut entries| entries.next().is_none())
        .unwrap_or(false);
    if empty {
        let _ = std::fs::remove_dir(target);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 翻转
// ─────────────────────────────────────────────────────────────────────────────

/// 把 staging 的内容翻进正式目录。
///
/// # 规则
///
/// * **文件** → 替换（`copy` + 删源）；
/// * **目录** → 递归合并；
/// * **包自己的目录**（npm 的 `node_modules\<包名>`）→ **整棵替换**：同一个包换版本时，
///   合并会在旧目录里留下新版本没有的文件，而"半个旧版本"比"没装"更难发现。
///   pip 那边没有这一条：它的 `site-packages` 是**所有包共用**的，整棵替换会删掉别人的文件。
fn flip(staging: &Path, target: &Path, install: &GlobalInstall) -> std::io::Result<()> {
    let replace: Vec<PathBuf> = match install.tool {
        GlobalsTool::Npm => vec![Path::new("node_modules").join(&install.name)],
        GlobalsTool::Pip => Vec::new(),
    };
    merge_dir(staging, target, Path::new(""), &replace)
}

/// 递归合并 `from` 到 `to`（`relative` 是相对 staging 的当前段，用来判"整棵替换"）。
fn merge_dir(from: &Path, to: &Path, relative: &Path, replace: &[PathBuf]) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        let source = entry.path();
        let destination = to.join(&name);
        let here = relative.join(&name);

        if source.is_dir() {
            if replace.iter().any(|wanted| wanted == &here) {
                // 整棵替换：先删旧的，再 `rename`（同一个卷，结构保证）。
                if destination.exists() {
                    std::fs::remove_dir_all(&destination)?;
                }
                std::fs::rename(&source, &destination)?;
                continue;
            }
            std::fs::create_dir_all(&destination)?;
            merge_dir(&source, &destination, &here, replace)?;
            continue;
        }
        // 文件（含没有扩展名的那些）：替换。
        std::fs::copy(&source, &destination)?;
        std::fs::remove_file(&source)?;
    }
    Ok(())
}

// ─────────────────────────────────────────────────────────────────────────────
// "东西真的装上了吗"（决策 197 的教训）
// ─────────────────────────────────────────────────────────────────────────────

/// staging 里真的有那个包吗。
///
/// npm：`<staging>\node_modules\<name>\package.json`（scope 里的 `/` 是路径分隔符）。
/// pip：某个 `site-packages` 目录里有 `<规范化包名>-<版本>.dist-info`（决策 198 的落点）。
fn artifact_present(install: &GlobalInstall, staging: &Path) -> bool {
    match install.tool {
        GlobalsTool::Npm => {
            let manifest = staging
                .join("node_modules")
                .join(install.name.replace('/', std::path::MAIN_SEPARATOR_STR))
                .join("package.json");
            let Ok(text) = std::fs::read_to_string(&manifest) else {
                return false;
            };
            match serde_json::from_str::<serde_json::Value>(&text) {
                // 清单读得出来：版本对得上才算（`version` 不在时**不判它**——
                // 那是"答不出来"，不是"答错了"）。
                Ok(value) => value
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|version| same_version(&install.version, version)),
                Err(_) => true,
            }
        }
        GlobalsTool::Pip => {
            let wanted = format!(
                "{}-{}.dist-info",
                normalize_pep503(&install.name),
                install.version
            )
            .to_ascii_lowercase();
            site_packages(staging, 0).into_iter().any(|dir| {
                std::fs::read_dir(&dir).is_ok_and(|entries| {
                    entries.flatten().any(|entry| {
                        entry.file_name().to_string_lossy().to_ascii_lowercase() == wanted
                    })
                })
            })
        }
    }
}

/// 找 staging 里的 `site-packages` 目录（**有界**：pip 自己插的那一层是 `Python312\`）。
fn site_packages(dir: &Path, depth: usize) -> Vec<PathBuf> {
    const MAX_DEPTH: usize = 3;
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case("site-packages")
        {
            found.push(entry.path());
            continue;
        }
        if depth < MAX_DEPTH {
            found.extend(site_packages(&entry.path(), depth + 1));
        }
    }
    found
}

/// PEP 503 的规范化：小写，`-` / `_` / `.` 的连续段压成一个 `-`。
///
/// `dist-info` 的目录名用的是这个形状（`zope.interface` → `zope_interface-…`）——
/// 不规范化就会把"装好了"判成"没装"。
fn normalize_pep503(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut pending = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            pending = !out.is_empty();
            continue;
        }
        if pending {
            out.push('-');
            pending = false;
        }
        out.extend(ch.to_lowercase());
    }
    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 报告里的子进程输出
// ─────────────────────────────────────────────────────────────────────────────

/// 报告里那一段输出的**长度上限**。
///
/// 截断是**说出来**的（末尾加 `...[truncated]`）：一个悄悄被剪短的错误信息，
/// 与"工具只说了这么多"长得一模一样。
const MAX_OUTPUT: usize = 4096;

/// 脱敏 + 压成 ASCII + 截断。
///
/// 顺序是刻意的：**先脱敏**（在原文上替换 token），再压 ASCII，最后截断 ——
/// 反过来会在替换之前就把一个形状打散。
fn redact_output(text: &str) -> (String, Vec<&'static str>) {
    let mut hits: Vec<&'static str> = Vec::new();
    let mut out = String::with_capacity(text.len());
    let mut span = String::new();
    for ch in text.chars() {
        if is_secret_delimiter(ch) {
            flush_span(&mut span, &mut out, &mut hits);
            out.push(ch);
        } else {
            span.push(ch);
        }
    }
    flush_span(&mut span, &mut out, &mut hits);

    // 复核：替换之后**还不干净**（私钥正文、`password = …` 这种没有厂商形状的）时，
    // 整段换成一个占位符 —— 宁可少说，也不漏材料。
    if let Some(detection) = secrets::scan_content(&out) {
        let slug = detection.shape.map_or("credential", secrets::Shape::as_str);
        return (format!("[redacted:{slug}]"), vec![slug]);
    }
    (out, hits)
}

/// [`secrets`] 切"词"用的那套分隔符（**与它一致**：`.` 与 `-` 不切，JWT 靠点分、`glpat-` 靠连字符）。
fn is_secret_delimiter(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '"' | '\'' | '<' | '>' | '=' | ':' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}'
        )
}

/// 一段"词"：像凭据就换成 `[redacted:<slug>]`，否则原样留下。
fn flush_span(span: &mut String, out: &mut String, hits: &mut Vec<&'static str>) {
    if span.is_empty() {
        return;
    }
    match secrets::value_shape(span) {
        Some(shape) => {
            hits.push(shape.as_str());
            out.push_str("[redacted:");
            out.push_str(shape.as_str());
            out.push(']');
        }
        None => out.push_str(span),
    }
    span.clear();
}

/// 报告里那一段输出：压成纯 ASCII（成功载荷不许本地化）+ 截断。
fn report_text(text: &str) -> String {
    let flattened = ascii_layout(text);
    if flattened.chars().count() <= MAX_OUTPUT {
        return flattened;
    }
    let head: String = flattened.chars().take(MAX_OUTPUT).collect();
    format!("{head}...[truncated]")
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use tuoen_platform::fixture::{FixtureDir, FixturePath, FixtureProcess, MachineFixture};

    use super::*;
    use crate::capture::files::GlobalPackage;
    use crate::detect::test_support::DetectFixture;

    const AT: &str = "2026-10-02T12:00:00Z";
    const BASE: &str = r"C:\Users\dev\AppData\Local\tuoen\globals";

    /// 一个工具的 `globals.toml` 行。
    fn row(
        tool: GlobalsTool,
        source: GlobalsSource,
        tool_version: &str,
        prefix: Option<&str>,
        packages: &[(&str, &str)],
    ) -> GlobalRow {
        GlobalRow {
            tool: tool.slug().to_owned(),
            source: source.slug().to_owned(),
            tool_version: tool_version.to_owned(),
            prefix: prefix.map(str::to_owned),
            prefix_inside_version_dir: prefix.map(|_| false),
            packages: packages
                .iter()
                .map(|(name, version)| GlobalPackage {
                    name: (*name).to_owned(),
                    version: (*version).to_owned(),
                })
                .collect(),
            enumerate_error: None,
        }
    }

    fn file(rows: Vec<GlobalRow>) -> GlobalsFile {
        let mut file = GlobalsFile::new(AT);
        file.global = rows;
        file
    }

    /// 本机现场：两个工具都在、npm 是 `v24.19.0`、我们的根里是 `packages`。
    fn local(packages: &[(&str, &str)]) -> GlobalsFile {
        file(vec![
            row(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                "v24.19.0",
                Some(r"C:\nvm4w\nodejs"),
                &[("pnpm", "11.21.0")],
            ),
            row(
                GlobalsTool::Npm,
                GlobalsSource::Tuoen,
                "v24.19.0",
                Some(&format!(r"{BASE}\npm\v24.19.0")),
                packages,
            ),
            row(
                GlobalsTool::Pip,
                GlobalsSource::Machine,
                "3.12",
                Some(r"C:\Python312"),
                &[("pypinyin", "0.55.0")],
            ),
        ])
    }

    fn base() -> PathBuf {
        PathBuf::from(BASE)
    }

    /// 快照：npm 的机器侧 4 个包。
    fn target() -> GlobalsFile {
        file(vec![row(
            GlobalsTool::Npm,
            GlobalsSource::Machine,
            "v24.19.0",
            Some(r"C:\nvm4w\nodejs"),
            &[
                ("corepack", "0.35.0"),
                ("esbuild", "0.25.0"),
                ("npm", "11.17.0"),
                ("pnpm", "11.21.0"),
            ],
        )])
    }

    /// **票据点名的那条固定装置用例**：本机 tuoen 根 3 个、快照 4 个 → 只装缺的那 1 个。
    #[test]
    fn three_present_and_four_wanted_installs_exactly_the_missing_one() {
        let wanted = globals_wanted(
            &target(),
            Some(&local(&[
                ("corepack", "0.35.0"),
                ("npm", "11.17.0"),
                ("pnpm", "11.21.0"),
            ])),
            Some(&base()),
            &BTreeMap::new(),
        );
        assert_eq!(
            wanted
                .installs
                .iter()
                .map(|install| install.name.as_str())
                .collect::<Vec<_>>(),
            ["esbuild"]
        );
        assert_eq!(wanted.already_present.len(), 3);
        assert_eq!(wanted.conflicts.len(), 0);
        assert!(
            wanted.needs_network(),
            "没探过缓存 ⇒ 一律算要网络（不许把'没探过'当成'缓存里有'）"
        );
        assert_eq!(
            wanted.installs[0].target,
            base().join("npm").join("v24.19.0"),
            "根名用**本机**运行时的原样版本"
        );
        assert_eq!(wanted.installs[0].sources, ["machine"]);
    }

    /// 缓存里能解决 ⇒ `needsNetwork = false`（票据 #24 §3）。
    #[test]
    fn a_probed_package_does_not_need_the_network() {
        let wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        assert_eq!(wanted.install_count(), 4);
        assert!(wanted.needs_network());

        let mut cache = BTreeMap::new();
        for install in &wanted.installs {
            cache.insert(
                cache_key(install.tool, &install.name, &install.version),
                true,
            );
        }
        let cached = globals_wanted(&target(), Some(&local(&[])), Some(&base()), &cache);
        assert!(!cached.needs_network(), "全都探到 ⇒ 不需要网络");
        assert_eq!(cached.network_packages(), Vec::<String>::new());
    }

    /// 两个来源版本不同 ⇒ **一个都不装**，而且两个版本都摆出来。
    #[test]
    fn two_sources_with_different_versions_install_neither() {
        let target = file(vec![
            row(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                "v24.19.0",
                Some(r"C:\nvm4w\nodejs"),
                &[("pnpm", "11.21.0")],
            ),
            row(
                GlobalsTool::Npm,
                GlobalsSource::Tuoen,
                "v24.19.0",
                Some(&format!(r"{BASE}\npm\v24.19.0")),
                &[("pnpm", "11.20.0")],
            ),
        ]);
        let wanted = globals_wanted(&target, Some(&local(&[])), Some(&base()), &BTreeMap::new());
        assert!(wanted.installs.is_empty(), "冲突的包一个都不装");
        assert_eq!(wanted.conflicts.len(), 1);
        assert_eq!(
            wanted.conflicts[0].versions,
            vec![
                ("machine", "11.21.0".to_owned()),
                ("tuoen", "11.20.0".to_owned())
            ],
            "两个版本都要摆出来（顺序固定）"
        );
    }

    /// 版本相同时**装一次**，来源说清楚。
    #[test]
    fn the_same_version_in_both_sources_is_one_install_with_two_sources() {
        let target = file(vec![
            row(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                "v24.19.0",
                Some(r"C:\nvm4w\nodejs"),
                &[("pnpm", "11.21.0")],
            ),
            row(
                GlobalsTool::Npm,
                GlobalsSource::Tuoen,
                "v24.19.0",
                Some(&format!(r"{BASE}\npm\v24.19.0")),
                &[("pnpm", "11.21.0")],
            ),
        ]);
        let wanted = globals_wanted(&target, Some(&local(&[])), Some(&base()), &BTreeMap::new());
        assert_eq!(wanted.installs.len(), 1);
        assert_eq!(wanted.installs[0].sources, ["machine", "tuoen"]);
    }

    /// 本机读不到我们自己的根（枚举失败）⇒ **不许**判成"什么都没有"⇒ 照装。
    #[test]
    fn an_unreadable_root_is_not_an_empty_root() {
        let mut local = local(&[("pnpm", "11.21.0")]);
        for row in &mut local.global {
            if row.source == GlobalsSource::Tuoen.slug() {
                row.enumerate_error = Some("command-failed".to_owned());
                row.packages.clear();
            }
        }
        let wanted = globals_wanted(&target(), Some(&local), Some(&base()), &BTreeMap::new());
        assert!(wanted.root_unreadable);
        assert_eq!(
            wanted.install_count(),
            4,
            "读不到根 ⇒ 当作全缺去装（宁可多装一遍）"
        );
    }

    /// 工具不在本机 `PATH` 上（本机侧一行都没有）⇒ 做不到，**不猜根名**。
    #[test]
    fn a_tool_that_is_not_here_is_unsupported() {
        let local = file(vec![row(
            GlobalsTool::Npm,
            GlobalsSource::Tuoen,
            "v24.19.0",
            None,
            &[],
        )]);
        let wanted = globals_wanted(&target(), Some(&local), Some(&base()), &BTreeMap::new());
        assert!(wanted.installs.is_empty());
        assert_eq!(wanted.unsupported.len(), 4);
        assert!(
            wanted
                .unsupported
                .iter()
                .all(|row| row.reason == "tool-not-on-path")
        );
    }

    /// 运行时版本答不出来 ⇒ npm 没有根（决策 26/172），**不产生 `…\npm\unknown\`**。
    #[test]
    fn an_unknown_runtime_version_has_no_npm_root() {
        let mut local = local(&[]);
        for row in &mut local.global {
            if row.tool == GlobalsTool::Npm.slug() && row.source == GlobalsSource::Machine.slug() {
                row.tool_version = UNKNOWN_VERSION.to_owned();
            }
        }
        let wanted = globals_wanted(&target(), Some(&local), Some(&base()), &BTreeMap::new());
        assert!(wanted.installs.is_empty());
        assert!(
            wanted
                .unsupported
                .iter()
                .all(|row| row.reason == "runtime-version-unknown")
        );
    }

    /// 算不出我们的根（没有 `%LOCALAPPDATA%`）⇒ 做不到，而且**不编一条前缀搬家的待办**。
    #[test]
    fn without_a_root_nothing_can_be_installed() {
        let wanted = globals_wanted(&target(), Some(&local(&[])), None, &BTreeMap::new());
        assert!(wanted.installs.is_empty());
        assert!(
            wanted
                .unsupported
                .iter()
                .all(|row| row.reason == "globals-root-unknown")
        );
        assert!(wanted.prefix_moved.is_empty(), "没有目的地就没有'搬到哪'");
    }

    /// 前缀搬家：快照里那个前缀不是我们装进去的地方（票据 #24 §4）。
    #[test]
    fn the_snapshots_prefix_is_reported_as_moved() {
        // 两个工具各一行：这一条说的是"**每个工具**最多一条"，所以固定装置里
        // 必须真的有第二个工具（只给 npm 时断言 2 条是在测固定装置，不是在测判据）。
        let target = file(vec![
            row(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                "v24.19.0",
                Some(r"C:\nvm4w\nodejs"),
                &[("pnpm", "11.21.0")],
            ),
            row(
                GlobalsTool::Pip,
                GlobalsSource::Machine,
                "3.12",
                Some(r"C:\Python312"),
                &[("pypinyin", "0.55.0")],
            ),
        ]);
        let wanted = globals_wanted(&target, Some(&local(&[])), Some(&base()), &BTreeMap::new());
        assert_eq!(wanted.prefix_moved.len(), 2, "npm 与 pip 各一条");
        let npm = &wanted.prefix_moved[0];
        assert_eq!(npm.tool, GlobalsTool::Npm);
        assert_eq!(npm.from, r"C:\nvm4w\nodejs");
        assert_eq!(npm.to, base().join("npm").join("v24.19.0"));
        let pip = &wanted.prefix_moved[1];
        assert_eq!(pip.tool, GlobalsTool::Pip);
        assert_eq!(pip.from, r"C:\Python312");
        assert_eq!(pip.to, base().join("pip"));
    }

    /// 快照里的前缀**就是**我们装进去的地方 ⇒ 没有"搬家"这回事。
    #[test]
    fn a_prefix_that_is_already_ours_is_not_reported_as_moved() {
        let target = file(vec![row(
            GlobalsTool::Npm,
            GlobalsSource::Machine,
            "v24.19.0",
            Some(&format!(r"{BASE}\npm\v24.19.0")),
            &[("pnpm", "11.21.0")],
        )]);
        let wanted = globals_wanted(&target, Some(&local(&[])), Some(&base()), &BTreeMap::new());
        assert!(wanted.prefix_moved.is_empty(), "{:?}", wanted.prefix_moved);
    }

    /// 本机根里多出来的包**只报数，从不卸载**。
    #[test]
    fn extra_packages_in_our_root_are_only_counted() {
        let wanted = globals_wanted(
            &target(),
            Some(&local(&[("something-else", "1.0.0")])),
            Some(&base()),
            &BTreeMap::new(),
        );
        assert_eq!(wanted.extra, vec!["npm:something-else".to_owned()]);
    }

    /// `--offline` + 需要网络 ⇒ **一个字节都不写**。
    #[test]
    fn offline_never_writes_a_package_that_needs_the_network() {
        let wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        let runner = FakeNpm::ok();
        let report = install_globals(&runner, &wanted, true);
        assert!(!report.wrote);
        assert!(
            report
                .packages
                .iter()
                .all(|package| package.result == RESULT_NOT_CACHED && package.output.is_none())
        );
        assert_eq!(report.failures.len(), 4);
        assert!(
            runner.calls.borrow().is_empty(),
            "拒绝的时候一个进程都不该起：{:?}",
            runner.calls.borrow()
        );
    }

    // ── 真装的固定装置：一个"会真的往 staging 里写文件"的假 npm ──────────────

    /// 假 npm：按参数分岔，**真的往 `--prefix` 指的目录里写产物**。
    ///
    /// 为什么不用 `FixtureProcess`：它只能回一段 canned 输出，而这一票要断言的恰恰是
    /// "产物真的出现了"（决策 197 的教训）与"翻转真的发生了" —— 那两件事需要文件。
    #[derive(Debug, Default)]
    struct FakeNpm {
        calls: RefCell<Vec<Vec<String>>>,
        /// 第几条安装命令要失败（0 起），失败时**先写半个产物**再非零退出。
        fail_at: Option<usize>,
        /// 退出 0 但**什么都不写**（决策 197 的 pip 形态）。
        silent: bool,
        installs: RefCell<usize>,
    }

    impl FakeNpm {
        fn ok() -> Self {
            Self::default()
        }

        fn failing_at(index: usize) -> Self {
            Self {
                fail_at: Some(index),
                ..Self::default()
            }
        }
    }

    impl ProcessRunner for FakeNpm {
        fn run_env(
            &self,
            program: &Path,
            args: &[&str],
            env: &[(String, String)],
            _timeout: Duration,
        ) -> ProcessOutcome {
            let mut all = vec![program.display().to_string()];
            all.extend(args.iter().map(|arg| (*arg).to_owned()));
            self.calls.borrow_mut().push(all);

            // `--prefix <dir> <spec>`：三段都在结尾（`Command::args` 的顺序）。
            let prefix = args
                .iter()
                .position(|arg| *arg == "--prefix")
                .and_then(|at| args.get(at + 1))
                .map(PathBuf::from)
                .expect("安装命令必须带 --prefix");
            let spec = args
                .iter()
                .find(|arg| arg.contains('@') || arg.contains("=="))
                .expect("安装命令必须有 spec");
            assert!(
                env.iter().any(|(name, value)| {
                    name == NPM_PREFIX_VAR && value == &prefix.to_string_lossy()
                }),
                "重定向变量必须指向同一个 staging：{env:?}"
            );

            let index = *self.installs.borrow();
            *self.installs.borrow_mut() += 1;
            let (name, version) = spec
                .split_once("==")
                .or_else(|| spec.split_once('@'))
                .expect("spec 是 name@version");

            // 先写"半个包"（`node_modules\<name>` 里已经有东西了）。
            let package = prefix.join("node_modules").join(name);
            std::fs::create_dir_all(&package).expect("建包目录");
            std::fs::write(package.join("README.md"), "half").expect("写半个产物");

            if self.silent {
                return outcome(0, "", "");
            }
            if self.fail_at == Some(index) {
                // 一个**不是** `ENOTCACHED` 的失败：那条 token 有自己的结果 slug
                // （`not-cached`），用它来测"中途失败"会让两条判据混在一起。
                return outcome(
                    1,
                    "",
                    "npm error code EACCES\nnpm error syscall mkdir\nnpm error path C:\\x\n",
                );
            }
            std::fs::write(
                package.join("package.json"),
                format!("{{\"name\":\"{name}\",\"version\":\"{version}\"}}"),
            )
            .expect("写 package.json");
            std::fs::write(prefix.join(format!("{name}.cmd")), "@echo off\r\n").expect("写 bin");
            outcome(0, "added 1 package\n", "")
        }
    }

    fn outcome(exit: i32, stdout: &str, stderr: &str) -> ProcessOutcome {
        ProcessOutcome {
            spawned: true,
            timed_out: false,
            exit_code: Some(exit),
            stdout_bytes: stdout.as_bytes().to_vec(),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
            spawn_error: None,
        }
    }

    /// 一个**真实的临时根**（`%TEMP%` 下自建自删）—— 测试绝不写真实的 tuoen 根。
    struct TempRoot {
        path: PathBuf,
    }

    impl TempRoot {
        fn new(label: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!(
                "tuoen-globals-install-{label}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("建临时根");
            Self { path }
        }
    }

    impl Drop for TempRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// 一棵目录树里的**相对路径 + 文件内容**（用来判"逐字节未变"）。
    fn tree(root: &Path) -> Vec<String> {
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                out.push(format!("{rel}/"));
                walk(root, &path, out);
            } else {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                out.push(format!("{rel}={text}"));
            }
        }
    }

    /// 把 `wanted` 的目标路径指到临时根里（固定装置里的 `C:\…` 是给断言用的形状）。
    fn retarget(wanted: &mut GlobalsWanted, root: &Path) {
        for install in &mut wanted.installs {
            install.target = root.join(install.tool.slug()).join(match install.tool {
                GlobalsTool::Npm => "v24.19.0",
                GlobalsTool::Pip => "pip",
            });
        }
    }

    /// 装成功：产物落进正式目录、staging 一个不剩。
    #[test]
    fn a_successful_install_flips_into_the_official_directory() {
        let temp = TempRoot::new("ok");
        let mut wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        retarget(&mut wanted, &temp.path);
        // 只装一个包，好让断言是"有限的那几个文件"。
        wanted.installs.truncate(1);
        let target_dir = wanted.installs[0].target.clone();

        let report = install_globals(&FakeNpm::ok(), &wanted, false);
        assert!(report.wrote);
        assert_eq!(report.installed_count(), 1);
        assert!(report.failures.is_empty(), "{:?}", report.failures);

        let name = &wanted.installs[0].name;
        assert!(
            target_dir
                .join("node_modules")
                .join(name)
                .join("package.json")
                .is_file(),
            "产物必须在正式目录里：{:?}",
            tree(&target_dir)
        );
        assert!(
            target_dir.join(format!("{name}.cmd")).is_file(),
            "顶层 bin 文件也要翻过去：{:?}",
            tree(&target_dir)
        );
        assert!(
            !tree(&target_dir)
                .iter()
                .any(|line| line.starts_with(STAGING_PREFIX)),
            "staging 不许留残余：{:?}",
            tree(&target_dir)
        );
        assert_eq!(
            report.packages[0].result, RESULT_INSTALLED,
            "{:?}",
            report.packages[0]
        );
    }

    /// 中间失败：**正式目录逐字节未变**，而且没有 `.staging-` 残余。
    #[test]
    fn a_failure_halfway_leaves_the_official_directory_untouched() {
        let temp = TempRoot::new("fail");
        let mut wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        retarget(&mut wanted, &temp.path);
        wanted.installs.truncate(1);
        let target_dir = wanted.installs[0].target.clone();

        // 正式目录**先有东西**（一个已经装好的包）—— "逐字节未变"才有内容可比。
        let existing = target_dir.join("node_modules").join("already-here");
        std::fs::create_dir_all(&existing).expect("建正式目录");
        std::fs::write(existing.join("package.json"), "{\"version\":\"9.9.9\"}").expect("写");
        let before = tree(&target_dir);

        let report = install_globals(&FakeNpm::failing_at(0), &wanted, false);
        assert!(!report.wrote);
        assert_eq!(report.packages[0].result, RESULT_INSTALL_FAILED);
        assert_eq!(
            report.packages[0].detail.as_deref(),
            Some("code=EACCES exit=1"),
            "判据来自机器 token，不是本地化的散文"
        );
        assert_eq!(tree(&target_dir), before, "正式目录必须逐字节未变");
        assert_eq!(report.failures.len(), 1);
    }

    /// 退出 0 但**一个字节都没写**（决策 197 的 pip 形态）⇒ `install-failed`，不是 `installed`。
    #[test]
    fn exit_zero_without_an_artifact_is_a_failure() {
        let temp = TempRoot::new("silent");
        let mut wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        retarget(&mut wanted, &temp.path);
        wanted.installs.truncate(1);
        let target_dir = wanted.installs[0].target.clone();

        let runner = FakeNpm {
            silent: true,
            ..FakeNpm::default()
        };
        let report = install_globals(&runner, &wanted, false);
        assert!(!report.wrote);
        assert_eq!(report.packages[0].result, RESULT_INSTALL_FAILED);
        assert_eq!(
            report.packages[0].detail.as_deref(),
            Some("reason=no-artifact"),
            "「命令成功」与「东西装上了」是两件事"
        );
        assert!(!target_dir.exists(), "一个空目录都不许留下");
    }

    /// `already-present` 与 `version-conflict` **都要可达**（apply 用同一个分类函数重算全集）。
    #[test]
    fn already_present_and_conflict_are_reachable_in_the_report() {
        let target = file(vec![
            row(
                GlobalsTool::Npm,
                GlobalsSource::Machine,
                "v24.19.0",
                Some(r"C:\nvm4w\nodejs"),
                &[("pnpm", "11.21.0"), ("corepack", "0.35.0")],
            ),
            row(
                GlobalsTool::Npm,
                GlobalsSource::Tuoen,
                "v24.19.0",
                None,
                &[("corepack", "0.34.0")],
            ),
        ]);
        let wanted = globals_wanted(
            &target,
            Some(&local(&[("pnpm", "11.21.0")])),
            Some(&base()),
            &BTreeMap::new(),
        );
        let report = install_globals(&FakeNpm::ok(), &wanted, true);
        let results: Vec<&str> = report
            .packages
            .iter()
            .map(|package| package.result)
            .collect();
        assert!(results.contains(&RESULT_ALREADY_PRESENT), "{results:?}");
        assert!(results.contains(&RESULT_VERSION_CONFLICT), "{results:?}");
        let conflict = report
            .packages
            .iter()
            .find(|package| package.result == RESULT_VERSION_CONFLICT)
            .expect("冲突那一行");
        assert_eq!(conflict.versions.len(), 2);
        assert_eq!(conflict.sources, ["machine", "tuoen"]);
    }

    /// 脱敏：厂商形状的 token 换成 `[redacted:<shape>]`，形状记进报告。
    #[test]
    fn a_credential_shaped_token_never_reaches_the_report() {
        // 前缀拼出来：源码里不留完整令牌（`secrets` 的固定装置同一条规矩）。
        let secret = format!("glpat-{}", "abcdefghijklmnopqrst");
        let (text, hits) = redact_output(&format!("npm error token {secret} rejected\n"));
        assert!(!text.contains(&secret), "{text}");
        assert!(text.contains("[redacted:gitlab-pat]"), "{text}");
        assert_eq!(hits, vec!["gitlab-pat"]);
        assert!(
            secrets::scan_content(&text).is_none(),
            "复核必须干净：{text}"
        );
    }

    /// 没有厂商形状但**看着像赋值**的那一种（`password = …`）：整段换掉（宁可少说）。
    #[test]
    fn an_assignment_shaped_secret_redacts_the_whole_output() {
        let (text, hits) = redact_output("password = hunter2hunter2\n");
        assert_eq!(text, "[redacted:credential]");
        assert_eq!(hits, vec!["credential"]);
    }

    /// 干净输出**一个字都不改**（否则"脱敏"会把正常的错误信息也吃掉）。
    #[test]
    fn a_clean_output_is_left_alone() {
        let (text, hits) = redact_output("npm error code ENOTCACHED\n");
        assert_eq!(text, "npm error code ENOTCACHED\n");
        assert!(hits.is_empty());
    }

    /// 探针：**只信真的成功**（没起来 / 超时 / 非零退出都算"缓存里没有"）。
    #[test]
    fn the_cache_probe_only_believes_a_real_success() {
        assert!(probe_says_cached(&outcome(0, "", "")));
        assert!(!probe_says_cached(&outcome(
            1,
            "",
            "npm error code ENOTCACHED\n"
        )));
        let mut timed_out = outcome(0, "", "");
        timed_out.timed_out = true;
        assert!(!probe_says_cached(&timed_out));
        let mut dead = outcome(0, "", "");
        dead.spawned = false;
        assert!(!probe_says_cached(&dead));
    }

    /// npm 的机器 token 从 `npm error code <CODE>` 里取（**不匹配本地化散文**）。
    #[test]
    fn the_npm_error_code_is_a_machine_token() {
        assert_eq!(
            npm_error_code("npm error code ENOTCACHED\nnpm error request to … failed\n").as_deref(),
            Some("ENOTCACHED")
        );
        assert_eq!(
            npm_error_code("npm error code E404\n").as_deref(),
            Some("E404")
        );
        assert_eq!(npm_error_code("安装失败：网络不可达\n"), None);
    }

    /// `ENOTCACHED` → `not-cached`；`E404`/`ETARGET` → `version-not-found`（**按 token 分**）。
    #[test]
    fn the_failure_mapping_goes_through_machine_tokens() {
        let mut wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        wanted.installs.truncate(1);
        let install = &wanted.installs[0];

        assert!(matches!(
            verdict_of(
                &outcome(1, "", "npm error code ENOTCACHED\n"),
                install,
                false
            ),
            Verdict::NotCached
        ));
        assert!(matches!(
            verdict_of(&outcome(1, "", "npm error code E404\n"), install, false),
            Verdict::VersionNotFound
        ));
        assert!(matches!(
            verdict_of(&outcome(1, "", "npm error code EACCES\n"), install, false),
            Verdict::Failed(_)
        ));
        let mut timed_out = outcome(1, "", "npm error code ENOTCACHED\n");
        timed_out.timed_out = true;
        assert!(matches!(
            verdict_of(&timed_out, install, false),
            Verdict::Failed(detail) if detail == "reason=timed-out"
        ));
    }

    /// pip 的 `dist-info` 目录名是 PEP 503 规范化的（判"装上了吗"要用它）。
    #[test]
    fn the_pep503_normalization_is_the_one_pip_uses() {
        assert_eq!(normalize_pep503("typing_extensions"), "typing-extensions");
        assert_eq!(normalize_pep503("zope.interface"), "zope-interface");
        assert_eq!(normalize_pep503("PyYAML"), "pyyaml");
        assert_eq!(normalize_pep503("pypinyin"), "pypinyin");
    }

    /// pip 的产物判据：某个 `site-packages` 里有那个 `dist-info`。
    #[test]
    fn a_pip_artifact_is_a_dist_info_directory() {
        let temp = TempRoot::new("pip");
        let install = GlobalInstall {
            tool: GlobalsTool::Pip,
            name: "pypinyin".to_owned(),
            version: "0.55.0".to_owned(),
            target: temp.path.clone(),
            sources: vec!["machine"],
            needs_network: true,
        };
        let dist = temp
            .path
            .join("Python312")
            .join("site-packages")
            .join("pypinyin-0.55.0.dist-info");
        assert!(!artifact_present(&install, &temp.path), "还没装");
        std::fs::create_dir_all(&dist).expect("建 dist-info");
        assert!(artifact_present(&install, &temp.path));
        // 版本不对 ⇒ 不算（"装上了"必须有版本的证据）。
        let other = GlobalInstall {
            version: "0.54.0".to_owned(),
            ..install
        };
        assert!(!artifact_present(&other, &temp.path));
    }

    /// 缓存探针：npm 不在 `PATH` 上时**一个进程都不起**。
    #[test]
    fn the_probe_starts_nothing_when_npm_is_not_on_path() {
        let description = MachineFixture {
            env: BTreeMap::from([("Path".to_owned(), r"C:\Windows".to_owned())]),
            ..MachineFixture::default()
        };
        let fixture = DetectFixture::build(&description);
        let wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        let facts = probe_npm_cache(&fixture.context(), &wanted.installs);
        assert!(facts.is_empty());
        assert!(
            fixture.machine.runner.calls().is_empty(),
            "找不到 npm 就不问"
        );
    }

    /// 缓存探针：真问的时候，参数是 `pack --dry-run --offline <spec>`，且**带 `--offline`**。
    #[test]
    fn the_probe_asks_npm_pack_dry_run_offline() {
        let description = MachineFixture {
            env: BTreeMap::from([("Path".to_owned(), r"C:\tools".to_owned())]),
            dirs: vec![FixtureDir::new(
                r"C:\tools",
                vec![FixturePath::file("npm.cmd", 340)],
            )],
            processes: vec![FixtureProcess {
                program: "cmd.exe".to_owned(),
                args: vec![
                    "/C".to_owned(),
                    "npm.cmd".to_owned(),
                    "pack".to_owned(),
                    "--dry-run".to_owned(),
                    "--offline".to_owned(),
                    "pnpm@11.21.0".to_owned(),
                ],
                stdout: "npm notice package: pnpm@11.21.0\n".to_owned(),
                stderr: String::new(),
                exit_code: Some(0),
                timed_out: false,
            }],
            ..MachineFixture::default()
        };
        let fixture = DetectFixture::build(&description);
        let mut wanted = globals_wanted(
            &target(),
            Some(&local(&[])),
            Some(&base()),
            &BTreeMap::new(),
        );
        wanted
            .installs
            .retain(|install| install.name == "pnpm" && install.tool == GlobalsTool::Npm);
        let facts = probe_npm_cache(&fixture.context(), &wanted.installs);
        assert_eq!(
            facts.get(&cache_key(GlobalsTool::Npm, "pnpm", "11.21.0")),
            Some(&true)
        );
        // 探到的结果进计划 ⇒ 那个包不再需要网络。
        let cached = globals_wanted(&target(), Some(&local(&[])), Some(&base()), &facts);
        assert!(
            cached
                .installs
                .iter()
                .find(|install| install.name == "pnpm")
                .is_some_and(|install| !install.needs_network)
        );
    }
}
