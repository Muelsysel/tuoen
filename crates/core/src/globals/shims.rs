//! 装好的包 → `PATH` 上要发的那些 `.exe` shim（票据 #25）。
//!
//! # 为什么这一节与 [`super::bins`] 分家
//!
//! `bins` 回答"这个包提供哪些命令名"（`binNames` 的唯一来源，**名字**的语义已经冻结）；
//! 这里回答"要把哪些名字变成 `PATH` 上的一个 `.exe`、指向什么、以及**为什么发不出来**"。
//! 两件事的消费者不同、失败后果不同：前者少一个名字只是报告差一行，后者少一条 shim
//! 是**用户敲不出来**。
//!
//! # npm 包的 shim 只有一种写法（机制强制的）
//!
//! `target = <store>\node\versions\<削过的版本>\node.exe`，
//! `prefix_args = [<包目录>\<bin 的相对路径>]`。
//!
//! 不能把 shim 直接指向那个 `.js` / `.mjs`：`ShimSpec::validate` 用
//! `ScriptKind::from_extension` 把脚本 target 拒成 `script-target`（决策 10 的那条
//! 事故：`CreateProcessW` 碰到脚本会走 `%COMSPEC% /c`，而 cmd 会剥掉整行的首尾引号）。
//! 所以"包 bin"这条路与 `npm` / `npx` / `corepack` 那张静态表**同构**：
//! 转发到 `node.exe`，把脚本路径作为**第一个前缀参数**。
//!
//! # 版本的**两个拼法**（决策 201）
//!
//! 同一个 Node 版本在本项目里有两处必须同时存在、写法却不同的地方：
//!
//! * **globals 根**用**原样**的 `v24.19.0`（`tuoen shell` 子进程里 `npm config get prefix`
//!   就是这个字符串，包就装在 `<globals>\npm\v24.19.0\node_modules\…`）；
//! * **store** 用**削过的** `24.19.0`（`nodes\versions\24.19.0\node.exe` 存在，
//!   `versions\v24.19.0\node.exe` **不存在** —— 实测）。
//!
//! 两者都来自**同一个** `GlobalInstall::tool_version`：原样那一份是 `install.target`
//! 的最后一段，削过的那一份由 `ToolSpec::strip_prefixes` 得到。**混用会指向一个不存在的
//! 目录**，而 `write_shim` 只检查 `spec.target` —— 于是症状是"shim 生成成功、敲命令时
//! 才发现目标不存在"，正是 `crate::shim` 的文档点名过的那个失效模式。
//!
//! # 决策 200：`node.exe` 必须来自**精确版本目录**
//!
//! 不许用 `store\node\current`（`tuoen use` 会移动它），更不许用机器自己的
//! `C:\nvm4w\nodejs\node.exe`（`nvm use` 会移动它）。否则用户切一次版本，
//! `v24.19.0` 根下的 `pnpm` shim 就会跑起**另一个** node —— 一个不报错的假话。
//! store 里没有那个精确版本 ⇒ [`NodeExe::Missing`]，**报告出来**，绝不静默退回。
//!
//! # 三条"说不出来就不说"的纪律
//!
//! 1. **前缀参数的存在性必须自己查。** `write_shim` 只检查 `spec.target`
//!    （`crates/shim/src/lib.rs` 的 `write_shim`），不检查前缀参数里那个 `.js` / `.mjs`
//!    在不在 —— 它甚至不该检查（那是 shim 层的职责边界）。所以这里查，
//!    查不到就是 [`REASON_BIN_MISSING`]。
//! 2. **绝不去读 `node_modules\.bin` 里的 `.cmd` / `.ps1`。** 那是 npm 生成的转发器，
//!    不是契约（实测 `pnpm` 那条 `.bin\pnpm.cmd` 是 538 字节的 `@echo off` 脚本）。
//!    命令名只从包自己的 `package.json` 与（pip 的）`Scripts` / `RECORD` 来。
//! 3. **被拒的名字必须报告。** `tuoen_shim::validate_name` 会拒空名字、含 `\` `/` `:`
//!    的名字、控制字符、以 `.` 或空格结尾、保留设备名 —— 每一条都要变成一条
//!    [`ShimIssue`]，**绝不许静默丢掉**：用户会以为那个命令发出去了。
//!
//! # `skipped-shadowed` 从哪来（票据 #24 冻结了 slug、本票产出它）
//!
//! 名字是 `PATH` 上的东西，所以"这条命令归谁"是一个**先到先得**的问题。三件不同的事
//! 在报告里必须分开：
//!
//! * 这个名字**我们本来就没发**（没查出来、目标不存在、名字非法）→ [`ShimIssue`]；
//! * 这个名字**已经有人占了**（另一个包/工具发过同名的 shim，或者盘上蹲着一个不是
//!   shim 的文件）→ `shadowed`，**不覆盖**，并在 `detail` 里点名被谁抢了；
//! * 这个名字**在我们的 shim 目录里还没有**，而且 `PATH` 上排在更前面的目录里有同名的
//!   `.cmd` / `.exe` → 那是 `tuoen_platform::detect_shadowing` 的活（`PATH` 级遮蔽），
//!   与"我们内部谁先占了名字"是两回事，**分开报**。
//!
//! 一个包里**一个名字都没发出来、且至少一条是因为被占** ⇒ 那个包的逐包 `result`
//! 记 [`super::RESULT_SKIPPED_SHADOWED`]。它**只**由这里产生（票据 #24 刻意不产出它）。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tuoen_shim::ShimSpec;

use crate::detect::DetectContext;
use crate::globals::bins;
use crate::globals::install::GlobalInstall;
use crate::globals::listing::GlobalsSource;
use crate::globals::root::GlobalsTool;
use crate::restore::manual::ascii_token;

/// 这个名字已经被别人占了（**不覆盖**）。`detail` 是 `by=<谁>`。
pub const REASON_SHADOWED: &str = "shadowed";
/// `bin` 里的那个值不是一个可用的**包内相对路径**（绝对路径 / `..` / 空）。
pub const REASON_NO_TARGET: &str = "no-target";
/// `bin` 指向的载荷在盘上不存在（`write_shim` 不会替我们查这一条）。
pub const REASON_BIN_MISSING: &str = "bin-missing";
/// store 里没有这个 Node 精确版本（决策 200：**不许**退回 `current` 或机器侧的 node）。
pub const REASON_NODE_MISSING: &str = "node-missing";
/// 那个版本号连拼进路径都不合法（store 的 `check_component` 拒了它）。
pub const REASON_NODE_VERSION_UNSAFE: &str = "node-version-unsafe";

/// 这个 npm 前缀对应的 `node.exe` —— **三态，不许合并**（决策 200）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeExe {
    /// store 的**精确版本目录**里有它。
    Found(PathBuf),
    /// store 里没有那个版本（或者那个目录里没有 `node.exe`）。
    Missing,
    /// 版本号那一串在 store 看来不合法（畸形版本号）—— 连拼路径都不该拼。
    UnsafeVersion(String),
}

/// 一个包里的**一条命令**的去向。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandDecision {
    /// 这一条该发（目标与前缀参数都已经在这里查过、验过）。
    Publish(ShimSpec),
    /// 这一条发不出来 —— `reason` 是稳定 slug，`detail` 是**纯 ASCII** 的补充数据。
    Rejected {
        /// 被拒的命令名。
        command: String,
        /// 稳定 slug（`tuoen_shim::ShimError::kind()` 或本模块的那几个常量）。
        reason: &'static str,
        /// 纯 ASCII 的补充数据（`by=…` / `path=…` / `version=…`）。
        detail: Option<String>,
    },
}

/// 一条**没能发出来**的命令 + 原因（进逐包结果，**不许静默丢**）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShimIssue {
    /// 命令名（可能是一个非法名字，所以是原样的字符串）。
    pub command: String,
    /// 稳定 slug：`shadowed` / `no-target` / `bin-missing` / `node-missing` /
    /// `node-version-unsafe`，或者 `tuoen_shim::ShimError::kind()` 的那一组。
    pub reason: &'static str,
    /// 纯 ASCII 的补充数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ShimIssue {
    /// 一条带原因与补充数据的记录。
    #[must_use]
    pub fn new(command: &str, reason: &'static str, detail: Option<String>) -> Self {
        Self {
            command: command.to_owned(),
            reason,
            detail,
        }
    }

    /// 这个名字是不是"被别人占了"（逐包 `skipped-shadowed` 的判据之一）。
    #[must_use]
    pub fn is_shadowed(&self) -> bool {
        self.reason == REASON_SHADOWED
    }
}

/// 一个包算完之后：**要发的**与**没发的**。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PackageShims {
    /// 要落盘的 shim（顺序 = 命令名排序，确定性是幂等性的一部分）。
    pub specs: Vec<ShimSpec>,
    /// 没发出来的每一条 + 为什么。
    pub issues: Vec<ShimIssue>,
}

impl PackageShims {
    /// 发出来的命令名（`specs` 的投影）—— 报告里 `shims` 那个键就是它。
    #[must_use]
    pub fn created(&self) -> Vec<String> {
        let mut names: Vec<String> = self.specs.iter().map(|spec| spec.name.clone()).collect();
        names.sort();
        names.dedup();
        names
    }

    /// 一个都没发出来、而且**至少有一条是因为被占**。
    ///
    /// 这就是逐包 `result = "skipped-shadowed"` 的判据：区分"一个都没发"
    /// （可能只是没查出来）与"被抢了"（用户需要知道**被谁**）。
    #[must_use]
    pub fn all_shadowed(&self) -> bool {
        self.specs.is_empty() && self.issues.iter().any(ShimIssue::is_shadowed)
    }
}

/// 一个名字**现在**在 shim 目录里归谁 —— 盘上的事实由调用方查
/// （`tuoen-core` 不认识 shim 的二进制格式；那一层在 `crates/shim` 与 CLI 里）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Occupied {
    /// 没有这个东西（该发就发）。
    Free,
    /// 盘上就是我们这一条命令（前缀逐字相同）—— 重写一遍，幂等。
    Same,
    /// 别人的东西（别人的 shim，或者一个根本不是我们的 shim 的文件）。
    /// `detail` 是**纯 ASCII** 的"被谁占"。
    Foreign(String),
}

// ─────────────────────────── node.exe 在哪（决策 200 / 201） ───────────────────────────

/// 那个 npm 前缀对应的 `node.exe`。
///
/// `npm_tool_version` 是**原样**的那一串（`v24.19.0` —— globals 根的最后一段就是它）。
/// **不是** store 里那一串：两个拼法在这里分开，削前缀只发生这一次。
#[must_use]
pub fn node_exe_for(store: &tuoen_store::Store, npm_tool_version: &str) -> NodeExe {
    // `node` 必然在 KNOWN_TOOLS 里；万一不在，我们**说不出来**（`Missing`），不乱猜。
    let Some(spec) = crate::detect::spec::spec_for_id("node") else {
        return NodeExe::Missing;
    };
    let stripped = spec.strip_prefixes(npm_tool_version).to_owned();
    // 先问判据再拼路径：`Store::version_dir` 对这一段是 `debug_assert!` +
    // `check_component`，一个来自 `node -v` 的畸形值在 debug 构建里会让进程 panic。
    if tuoen_store::layout::check_component("版本号", &stripped).is_err() {
        return NodeExe::UnsafeVersion(stripped);
    }
    match tuoen_store::find_version(store, "node", &stripped) {
        Some(found) => {
            let exe = found.path.join("node.exe");
            if exe.is_file() {
                NodeExe::Found(exe)
            } else {
                // 版本目录在、`node.exe` 不在：那是**目录的事实**，不是版本不存在，
                // 而报告要说的都是同一句话"这一版没有可用的 node"。
                NodeExe::Missing
            }
        }
        None => NodeExe::Missing,
    }
}

// ─────────────────────────── 一个包 → 要发哪些命令 ───────────────────────────

/// 一个装好的包**应该**发哪些 shim（还没有判"名字有没有被占"）。
#[must_use]
pub fn commands_for(
    ctx: &DetectContext<'_>,
    install: &GlobalInstall,
    node: &NodeExe,
) -> Vec<CommandDecision> {
    match install.tool {
        GlobalsTool::Npm => npm_commands(ctx, install, node),
        GlobalsTool::Pip => pip_commands(ctx, install),
    }
}

/// npm：名字来自包自己的 `bin`，目标是 `node.exe` + 那个脚本的绝对路径。
fn npm_commands(
    ctx: &DetectContext<'_>,
    install: &GlobalInstall,
    node: &NodeExe,
) -> Vec<CommandDecision> {
    let prefix = &install.target;
    // 名字用 `binNames` 那一条路（键的投影，语义已冻结），目标用 `npm_bin_entries`。
    // 两次读取同一份 `package.json` 是**故意的**：合成一个函数会让 `binNames`
    // 的语义跟着 shim 的需求走，而它已经冻结（改它要递增 `schemaVersion`）。
    let names = bins::bin_names(
        ctx,
        GlobalsTool::Npm,
        GlobalsSource::Tuoen,
        prefix,
        &install.tool_version,
        &install.name,
    );
    let targets: BTreeMap<String, String> = bins::npm_bin_entries(ctx, prefix, &install.name)
        .into_iter()
        .collect();

    let package_dir = prefix.join("node_modules").join(&install.name);
    let mut out = Vec::new();
    for name in names {
        let Some(relative) = targets.get(&name) else {
            // `bin` 里有这个名字，但它的值不是一个可用的包内相对路径。
            out.push(reject(&name, REASON_NO_TARGET, None));
            continue;
        };
        let node_exe = match node {
            NodeExe::Found(path) => path,
            NodeExe::Missing => {
                out.push(reject(
                    &name,
                    REASON_NODE_MISSING,
                    Some(format!("version={}", ascii_token(&install.tool_version))),
                ));
                continue;
            }
            NodeExe::UnsafeVersion(version) => {
                out.push(reject(
                    &name,
                    REASON_NODE_VERSION_UNSAFE,
                    Some(format!("version={}", ascii_token(version))),
                ));
                continue;
            }
        };
        let payload = package_dir.join(relative);
        if !is_file(ctx, &payload) {
            // **`write_shim` 不会替我们查这一条**（它只看 `spec.target`）。
            out.push(reject(
                &name,
                REASON_BIN_MISSING,
                Some(format!("path={}", ascii_token(&payload.to_string_lossy()))),
            ));
            continue;
        }
        out.push(publish(name, node_exe.clone(), vec![payload]));
    }
    out
}

/// pip：`Scripts\*.exe` 就是命令本身 —— 一个前缀参数都不需要。
fn pip_commands(ctx: &DetectContext<'_>, install: &GlobalInstall) -> Vec<CommandDecision> {
    let prefix = &install.target;
    let Some(scripts) = bins::pip_scripts_dir(GlobalsSource::Tuoen, prefix, &install.tool_version)
    else {
        // python 版本答不上来时那一层目录名就是猜的 —— 一个名字都不发（不猜）。
        return Vec::new();
    };
    let names = bins::bin_names(
        ctx,
        GlobalsTool::Pip,
        GlobalsSource::Tuoen,
        prefix,
        &install.tool_version,
        &install.name,
    );
    let mut out = Vec::new();
    for file in names {
        let Some(command) = bins::strip_exe_extension(&file) else {
            continue;
        };
        let target = scripts.join(&file);
        if !is_file(ctx, &target) {
            out.push(reject(
                command,
                REASON_BIN_MISSING,
                Some(format!("path={}", ascii_token(&target.to_string_lossy()))),
            ));
            continue;
        }
        out.push(publish(command.to_owned(), target, Vec::new()));
    }
    out
}

/// 拼一条 `Publish`：**在这里**跑一次 `ShimSpec::validate`。
///
/// 这是"名字非法"唯一能被抓住的地方 —— 而它必须被抓住，因为一条非法的 shim 名字
/// 会在落盘时被 `write_shim` 拒成 `bad-name`，然后**那一条命令就凭空消失了**。
fn publish(name: String, target: PathBuf, prefix_args: Vec<PathBuf>) -> CommandDecision {
    let spec = ShimSpec {
        name,
        target,
        prefix_args: prefix_args
            .into_iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect(),
    };
    match spec.validate() {
        Ok(()) => CommandDecision::Publish(spec),
        Err(error) => CommandDecision::Rejected {
            command: spec.name.clone(),
            reason: error.kind(),
            detail: Some(format!("shim={}", ascii_token(&error.to_string()))),
        },
    }
}

/// 拼一条 `Rejected`。
fn reject(command: &str, reason: &'static str, detail: Option<String>) -> CommandDecision {
    CommandDecision::Rejected {
        command: command.to_owned(),
        reason,
        detail,
    }
}

/// 这个路径现在是不是一个**文件**（`exists` + 不是目录）。
///
/// 用 `ctx.fs` 而不是 `std::fs`：这条判据必须能在固定装置上被测到
/// （"目标不存在"那个失效模式正是本票要堵的洞）。
fn is_file(ctx: &DetectContext<'_>, path: &std::path::Path) -> bool {
    let facts = ctx.fs.inspect(path);
    facts.exists && !facts.is_dir
}

// ─────────────────────────── 名字的归属：先到先得 ───────────────────────────

/// 把"该发什么"收成"真的发什么"：**同一个名字只能有一个主人**。
///
/// `claims` 是**这一趟**已经发出过名字的记录（名字小写 → 主人），跨包共享；
/// 调用方每处理一个包就把它带着往下传，于是"同一个快照里的两个包抢同一个名字"
/// 会被第二个包如实报成 `shadowed`（而 `lookup` 看不见这一层 —— 那时盘上还没有）。
///
/// `owner` 是这一个包的名字（进 `by=`，例如 `package:npm:pnpm`）。
/// `lookup` 回答"这个名字在盘上现在归谁" —— 由调用方查（要读 shim 的二进制）。
#[must_use]
pub fn resolve(
    decisions: Vec<CommandDecision>,
    owner: &str,
    claims: &mut BTreeMap<String, String>,
    lookup: &dyn Fn(&ShimSpec) -> Occupied,
) -> PackageShims {
    let mut out = PackageShims::default();
    for decision in decisions {
        match decision {
            CommandDecision::Rejected {
                command,
                reason,
                detail,
            } => out.issues.push(ShimIssue {
                command,
                reason,
                detail,
            }),
            CommandDecision::Publish(spec) => {
                let key = spec.name.to_lowercase();
                if let Some(previous) = claims.get(&key) {
                    // 这一趟里先发的那个包赢了：**后发的不覆盖**（顺序是稳定的，
                    // 所以"谁赢"也是稳定的，不会这次一个下次另一个）。
                    out.issues.push(ShimIssue::new(
                        &spec.name,
                        REASON_SHADOWED,
                        Some(format!("by={}", ascii_token(previous))),
                    ));
                    continue;
                }
                match lookup(&spec) {
                    Occupied::Free | Occupied::Same => {
                        claims.insert(key, owner.to_owned());
                        out.specs.push(spec);
                    }
                    // `by` 由调用方给（可能是一条路径）—— **在这里统一压成 ASCII**，
                    // 免得每一个调用方各记一次"要脱 CJK"这件事（`detail` 是 `--json` 的一部分）。
                    Occupied::Foreign(by) => out.issues.push(ShimIssue::new(
                        &spec.name,
                        REASON_SHADOWED,
                        Some(format!("by={}", ascii_token(&by))),
                    )),
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tuoen_platform::fixture::{FixtureDir, FixturePath, MachineFixture};
    use tuoen_shim::ShimSpec;

    use super::*;
    use crate::detect::test_support::DetectFixture;
    use crate::restore::test_support::TempDir;

    /// 真机上那个 npm 前缀的形状：`<base>\npm\<node -v 的**原样**输出>`。
    const NPM_PREFIX: &str = r"C:\globals\npm\v24.19.0";
    /// 真机上那个 pip 前缀的形状：`<base>\pip`。
    const PIP_PREFIX: &str = r"C:\globals\pip";
    /// store 里那个**削过的**版本目录（决策 201 的第二个拼法）。
    const STORE_NODE: &str = r"C:\store\node\versions\24.19.0\node.exe";

    /// 一个包自己的 `package.json`（真机形状：`pnpm@11.21.0` 的四个名字）。
    const PNPM_MANIFEST: &str = r#"{"name":"pnpm","version":"11.21.0","bin":{"pnpm":"bin/pnpm.mjs","pnpx":"bin/pnpx.mjs","pn":"bin/pnpm.mjs","pnx":"bin/pnpx.mjs"}}"#;
    /// `bin` 是字符串的作用域包 —— 命令名是包名（不带 scope）。
    const SCOPED_MANIFEST: &str = r#"{"name":"@scope/thing","version":"2.0.0","bin":"cli.js"}"#;
    /// 目标带着 `./` 前臂（真机 `corepack@0.35.0` 的五个值全是这样）。
    const COREPACK_MANIFEST: &str =
        r#"{"name":"corepack","version":"0.35.0","bin":{"corepack":"./dist/corepack.js"}}"#;
    /// 目标不在包内（绝对路径 / `..` / 空）。
    const BAD_TARGETS: &str = r#"{"name":"badtargets","version":"1.0.0","bin":{"absolute":"C:/evil.js","escape":"../x.js","empty":""}}"#;
    /// 名字是 Windows 保留设备名 —— 目标**真的存在**，所以被拒的理由只能是名字。
    const BAD_NAME: &str = r#"{"name":"badname","version":"1.0.0","bin":{"CON":"real.js"}}"#;

    fn machine() -> MachineFixture {
        MachineFixture {
            dirs: vec![
                FixtureDir::new(
                    &format!(r"{NPM_PREFIX}\node_modules\pnpm"),
                    vec![
                        FixturePath::file_with_content("package.json", PNPM_MANIFEST),
                        FixturePath::file_with_content(r"bin\pnpm.mjs", "export {};\n"),
                        FixturePath::file_with_content(r"bin\pnpx.mjs", "export {};\n"),
                    ],
                ),
                FixtureDir::new(
                    &format!(r"{NPM_PREFIX}\node_modules\@scope\thing"),
                    vec![
                        FixturePath::file_with_content("package.json", SCOPED_MANIFEST),
                        FixturePath::file_with_content("cli.js", "#!/usr/bin/env node\n"),
                    ],
                ),
                FixtureDir::new(
                    &format!(r"{NPM_PREFIX}\node_modules\corepack"),
                    vec![
                        FixturePath::file_with_content("package.json", COREPACK_MANIFEST),
                        FixturePath::file_with_content(r"dist\corepack.js", "export {};\n"),
                    ],
                ),
                FixtureDir::new(
                    &format!(r"{NPM_PREFIX}\node_modules\badtargets"),
                    vec![FixturePath::file_with_content("package.json", BAD_TARGETS)],
                ),
                FixtureDir::new(
                    &format!(r"{NPM_PREFIX}\node_modules\badname"),
                    vec![
                        FixturePath::file_with_content("package.json", BAD_NAME),
                        FixturePath::file_with_content("real.js", "export {};\n"),
                    ],
                ),
                // `bin` 说在那儿、盘上没有：`ghost` 那一个名字要报 `bin-missing`。
                FixtureDir::new(
                    &format!(r"{NPM_PREFIX}\node_modules\ghost"),
                    vec![FixturePath::file_with_content(
                        "package.json",
                        r#"{"name":"ghost","version":"1.0.0","bin":{"ghost":"missing.js"}}"#,
                    )],
                ),
                FixtureDir::new(
                    &format!(r"{PIP_PREFIX}\Python312\Scripts"),
                    vec![
                        FixturePath::file("pip.exe", 108_425),
                        FixturePath::file("pip3.12.exe", 108_425),
                        FixturePath::file("pypinyin.exe", 108_420),
                        FixturePath::file("not-an-exe.txt", 10),
                    ],
                ),
            ],
            ..MachineFixture::default()
        }
    }

    fn install(tool: GlobalsTool, name: &str, target: &str, tool_version: &str) -> GlobalInstall {
        GlobalInstall {
            tool,
            name: name.to_owned(),
            version: "1.0.0".to_owned(),
            target: PathBuf::from(target),
            tool_version: tool_version.to_owned(),
            sources: vec!["machine"],
            needs_network: false,
        }
    }

    fn found_node() -> NodeExe {
        NodeExe::Found(PathBuf::from(STORE_NODE))
    }

    /// 这一组决定里"要发的那些"。
    fn specs(decisions: &[CommandDecision]) -> Vec<ShimSpec> {
        decisions
            .iter()
            .filter_map(|decision| match decision {
                CommandDecision::Publish(spec) => Some(spec.clone()),
                CommandDecision::Rejected { .. } => None,
            })
            .collect()
    }

    /// 这一组决定里"没发的那些"：`(名字, reason, detail)`。
    fn rejects(decisions: &[CommandDecision]) -> Vec<(String, &'static str, Option<String>)> {
        decisions
            .iter()
            .filter_map(|decision| match decision {
                CommandDecision::Rejected {
                    command,
                    reason,
                    detail,
                } => Some((command.clone(), *reason, detail.clone())),
                CommandDecision::Publish(_) => None,
            })
            .collect()
    }

    /// npm：包自己的 `bin` 有几条命令，就发几条 —— **目标是 `node.exe`，
    /// 脚本路径是第一个前缀参数**（脚本 target 会被 `ShimSpec::validate` 拒成
    /// `script-target`，这是机制强制的写法）。
    #[test]
    fn an_npm_package_turns_its_bin_entries_into_node_forwarders() {
        let fixture = DetectFixture::build(&machine());
        let decisions = commands_for(
            &fixture.context(),
            &install(GlobalsTool::Npm, "pnpm", NPM_PREFIX, "v24.19.0"),
            &found_node(),
        );
        let specs = specs(&decisions);
        assert_eq!(
            specs
                .iter()
                .map(|spec| spec.name.as_str())
                .collect::<Vec<_>>(),
            ["pn", "pnpm", "pnpx", "pnx"]
        );
        for spec in &specs {
            assert_eq!(
                spec.target,
                PathBuf::from(STORE_NODE),
                "目标是 store 里那个**精确版本目录**的 node.exe（决策 200）"
            );
            assert_eq!(spec.prefix_args.len(), 1, "前缀参数就是那个脚本：{spec:?}");
            assert!(
                spec.prefix_args[0].ends_with(".mjs"),
                "前缀参数必须是包目录里的那个脚本：{spec:?}"
            );
        }
        // `pn` 与 `pnx` 是别名：它们指向**别的**脚本，而不是同一个。
        let pnpm = specs.iter().find(|spec| spec.name == "pnpm").unwrap();
        let pnpx = specs.iter().find(|spec| spec.name == "pnpx").unwrap();
        assert!(pnpm.prefix_args[0].ends_with(r"bin\pnpm.mjs"));
        assert!(pnpx.prefix_args[0].ends_with(r"bin\pnpx.mjs"));
    }

    /// `bin` 的目标先归一化（`./`）再拼成绝对路径 —— 拼错一步就是
    /// "shim 生成成功、敲命令时才发现目标不存在"。
    #[test]
    fn a_leading_dot_slash_in_the_bin_target_is_normalized() {
        let fixture = DetectFixture::build(&machine());
        let decisions = commands_for(
            &fixture.context(),
            &install(GlobalsTool::Npm, "corepack", NPM_PREFIX, "v24.19.0"),
            &found_node(),
        );
        let specs = specs(&decisions);
        assert_eq!(specs.len(), 1, "{decisions:?}");
        assert_eq!(
            specs[0].prefix_args,
            [format!(
                r"{NPM_PREFIX}\node_modules\corepack\dist\corepack.js"
            )]
        );
    }

    /// `bin` 是字符串（作用域包）⇒ 一条命令，名字是**包名**（不带 scope）。
    #[test]
    fn a_scoped_package_with_a_string_bin_becomes_one_command() {
        let fixture = DetectFixture::build(&machine());
        let decisions = commands_for(
            &fixture.context(),
            &install(GlobalsTool::Npm, "@scope/thing", NPM_PREFIX, "v24.19.0"),
            &found_node(),
        );
        let specs = specs(&decisions);
        assert_eq!(specs.len(), 1, "{decisions:?}");
        assert_eq!(
            specs[0].name, "thing",
            "scope 不进命令名（`tuoen_shim::validate_name` 会因为 `/` 拒掉它）"
        );
    }

    /// **前缀参数的存在性必须我们查**（`write_shim` 只看 `spec.target`）。
    #[test]
    fn a_bin_target_that_is_not_on_disk_is_reported_rather_than_published() {
        let fixture = DetectFixture::build(&machine());
        let decisions = commands_for(
            &fixture.context(),
            &install(GlobalsTool::Npm, "ghost", NPM_PREFIX, "v24.19.0"),
            &found_node(),
        );
        assert!(specs(&decisions).is_empty(), "{decisions:?}");
        let rejects = rejects(&decisions);
        assert_eq!(rejects.len(), 1);
        assert_eq!(rejects[0].0, "ghost");
        assert_eq!(rejects[0].1, REASON_BIN_MISSING);
        let detail = rejects[0].2.clone().unwrap_or_default();
        assert!(
            detail.contains("missing.js"),
            "detail 要点名**哪一个**载荷不在：{detail}"
        );
        assert!(detail.is_ascii(), "detail 必须纯 ASCII：{detail}");
    }

    /// `bin` 的值不是包内相对路径 ⇒ 一条命令都不许发（那是"包外的东西"）。
    #[test]
    fn a_bin_target_that_points_outside_the_package_is_reported() {
        let fixture = DetectFixture::build(&machine());
        let decisions = commands_for(
            &fixture.context(),
            &install(GlobalsTool::Npm, "badtargets", NPM_PREFIX, "v24.19.0"),
            &found_node(),
        );
        assert!(specs(&decisions).is_empty(), "{decisions:?}");
        let rejects = rejects(&decisions);
        assert_eq!(rejects.len(), 3, "三个名字都要报出来，一个都不许静默丢");
        assert!(
            rejects
                .iter()
                .all(|(_, reason, _)| *reason == REASON_NO_TARGET),
            "{rejects:?}"
        );
    }

    /// 名字非法（保留设备名）⇒ 报告 `bad-name`（`ShimError::kind()`），
    /// **而不是**在落盘时才失败、让那条命令凭空消失。
    #[test]
    fn a_bin_name_windows_would_not_accept_is_reported() {
        let fixture = DetectFixture::build(&machine());
        let decisions = commands_for(
            &fixture.context(),
            &install(GlobalsTool::Npm, "badname", NPM_PREFIX, "v24.19.0"),
            &found_node(),
        );
        assert!(specs(&decisions).is_empty(), "{decisions:?}");
        let rejects = rejects(&decisions);
        assert_eq!(rejects[0].0, "CON");
        assert_eq!(rejects[0].1, "bad-name", "slug 用 shim  crate 那一套");
    }

    /// store 里没有那个精确版本 ⇒ **报告**，绝不退回 `current` 或机器侧的 node
    /// （决策 200：那会是一个"跑着另一个 node 而不报错"的假话）。
    #[test]
    fn a_missing_node_exe_is_reported_and_never_falls_back() {
        let fixture = DetectFixture::build(&machine());
        for node in [NodeExe::Missing, NodeExe::UnsafeVersion("x/y".to_owned())] {
            let decisions = commands_for(
                &fixture.context(),
                &install(GlobalsTool::Npm, "pnpm", NPM_PREFIX, "v24.19.0"),
                &node,
            );
            assert!(specs(&decisions).is_empty(), "{decisions:?}");
            let rejects = rejects(&decisions);
            assert_eq!(rejects.len(), 4, "四条命令都要说清为什么没发：{rejects:?}");
            assert!(rejects.iter().all(|(_, reason, _)| *reason
                == match node {
                    NodeExe::Missing => REASON_NODE_MISSING,
                    _ => REASON_NODE_VERSION_UNSAFE,
                }));
        }
    }

    /// pip：`Scripts\*.exe` 就是命令本身，**一个前缀参数都不需要**；
    /// `pip3.12` 这种带点的名字是合法的（`validate_name` 只拒结尾的点）。
    #[test]
    fn pip_publishes_the_scripts_directory_as_commands() {
        let fixture = DetectFixture::build(&machine());
        for (package, expected) in [("pip", "pip"), ("pypinyin", "pypinyin")] {
            let decisions = commands_for(
                &fixture.context(),
                &install(GlobalsTool::Pip, package, PIP_PREFIX, "3.12"),
                &NodeExe::Missing, // pip 这条路根本不看 node
            );
            let specs = specs(&decisions);
            assert_eq!(specs.len(), 1, "{package}: {decisions:?}");
            assert_eq!(specs[0].name, expected);
            assert!(specs[0].prefix_args.is_empty());
            assert_eq!(
                specs[0].target,
                PathBuf::from(format!(r"{PIP_PREFIX}\Python312\Scripts\{expected}.exe"))
            );
        }
    }

    /// `pip3.12` 里那个点**不是**结尾的点，所以合法 —— 用一个直接的名字用例钉住它
    /// （真机上 `Scripts\pip3.12.exe` 就在那儿）。
    #[test]
    fn a_pip_command_may_contain_a_dot() {
        // 这条名字只能来自固定装置里真的有一个 `pip3.12.exe`；`pip` 那个包的
        // 精确名字匹配只命中 `pip.exe`，所以这里直接问 `validate_name`。
        assert!(
            tuoen_shim::validate_name("pip3.12").is_ok(),
            "`pip3.12` 必须是合法名字（决策：只有**结尾**的点与空格才拒）"
        );
        assert!(tuoen_shim::validate_name("pip3.12.").is_err());
    }

    /// 先到先得：同一个名字，先发的那个包赢，后发的报 `shadowed` 并**点名**是谁。
    #[test]
    fn the_first_package_owns_the_name_and_the_second_is_shadowed() {
        let fixture = DetectFixture::build(&machine());
        let ctx = fixture.context();
        // 盘上什么都没有。
        let lookup = |_: &ShimSpec| Occupied::Free;

        let mut claims = BTreeMap::new();
        let pnpm = install(GlobalsTool::Npm, "pnpm", NPM_PREFIX, "v24.19.0");
        let first = resolve(
            commands_for(&ctx, &pnpm, &found_node()),
            "package:npm:pnpm",
            &mut claims,
            &lookup,
        );
        assert_eq!(first.created(), ["pn", "pnpm", "pnpx", "pnx"]);
        assert!(first.issues.is_empty(), "{:?}", first.issues);

        // 另一个包想要同一个名字（真机上 machine 侧 npm 的 `npx` 就是这个局面）。
        let other = GlobalInstall {
            name: "npm".to_owned(),
            ..pnpm.clone()
        };
        let mut decisions = commands_for(&ctx, &other, &found_node());
        // 给这个假包一条与 `pnpm` 撞名的命令（真实冲突素材的形状）。
        decisions.push(CommandDecision::Publish(ShimSpec {
            name: "pnpm".to_owned(),
            target: PathBuf::from(STORE_NODE),
            prefix_args: Vec::new(),
        }));
        let second = resolve(decisions, "package:npm:npm", &mut claims, &lookup);

        let shadowed: Vec<&ShimIssue> = second
            .issues
            .iter()
            .filter(|issue| issue.is_shadowed())
            .collect();
        assert_eq!(shadowed.len(), 1, "{:?}", second.issues);
        assert_eq!(shadowed[0].command, "pnpm");
        assert_eq!(
            shadowed[0].detail.as_deref(),
            Some("by=package:npm:pnpm"),
            "必须说清被**谁**抢了"
        );
        assert!(
            !second.created().contains(&"pnpm".to_owned()),
            "别人的名字不许覆盖过去"
        );
    }

    /// 盘上那条命令是我们自己的（前缀逐字相同）⇒ 重写一遍（幂等）；
    /// 是别人的 ⇒ 一个字都不动，报 `shadowed`。
    #[test]
    fn our_own_command_is_rewritten_and_someone_elses_is_left_alone() {
        let fixture = DetectFixture::build(&machine());
        let ctx = fixture.context();
        let pnpm = install(GlobalsTool::Npm, "pnpm", NPM_PREFIX, "v24.19.0");
        let decisions = commands_for(&ctx, &pnpm, &found_node());

        // 全是我们自己的（`Same`）→ 全都重写。
        let all_same = resolve(
            decisions.clone(),
            "package:npm:pnpm",
            &mut BTreeMap::new(),
            &|_: &ShimSpec| Occupied::Same,
        );
        assert_eq!(all_same.created(), ["pn", "pnpm", "pnpx", "pnx"]);
        assert!(all_same.issues.is_empty());

        // 全是别人的（`Foreign`）→ 一条都不发，逐条点名。
        let mut claims = BTreeMap::new();
        let foreign = resolve(
            decisions,
            "package:npm:pnpm",
            &mut claims,
            &|spec: &ShimSpec| Occupied::Foreign(format!("file:{}.exe", spec.name)),
        );
        assert!(foreign.created().is_empty());
        assert_eq!(foreign.issues.len(), 4);
        assert!(foreign.all_shadowed(), "一条都没发、且都是被占");
        assert!(claims.is_empty(), "没发出去的名字**不许**记成自己的");
        assert_eq!(
            foreign.issues[0].detail.as_deref(),
            Some("by=file:pn.exe"),
            "detail 说清被谁占"
        );
    }

    /// "一个都没发"与"被抢了"是两件事：只有后者才是 `skipped-shadowed`。
    #[test]
    fn all_shadowed_needs_a_shadow_and_not_just_an_empty_result() {
        let empty = PackageShims::default();
        assert!(!empty.all_shadowed(), "一个名字都没查出来不是'被抢了'");

        let issues_only = PackageShims {
            specs: Vec::new(),
            issues: vec![ShimIssue::new("ghost", REASON_BIN_MISSING, None)],
        };
        assert!(!issues_only.all_shadowed(), "发不出来不等于被别人占了");

        let shadowed = PackageShims {
            specs: Vec::new(),
            issues: vec![ShimIssue::new(
                "pnpm",
                REASON_SHADOWED,
                Some("by=x".to_owned()),
            )],
        };
        assert!(shadowed.all_shadowed());
    }

    /// store 里那个目录是**削过的**版本号：`24.19.0`。放一个 `v24.19.0` 的诱饵
    /// 在旁边，断言我们**没有**取它（决策 201 的第二个拼法）。
    #[test]
    fn the_node_exe_comes_from_the_stripped_version_directory_only() {
        let temp = TempDir::new("shims-node");
        let store = tuoen_store::Store::new(temp.path());
        // 正主：`versions\24.19.0\node.exe`。
        temp.write(r"node\versions\24.19.0\node.exe", "MZ");
        // 诱饵：`versions\v24.19.0\node.exe`（真机上**不存在**，这里故意造出来）。
        let decoy = temp.write(r"node\versions\v24.19.0\node.exe", "MZ");

        let got = node_exe_for(&store, "v24.19.0");
        let NodeExe::Found(path) = &got else {
            panic!("必须找到那一版：{got:?}");
        };
        assert_eq!(path, &temp.path().join(r"node\versions\24.19.0\node.exe"));
        assert_ne!(path, &decoy, "`v24.19.0` 那个目录是诱饵，不许取");
    }

    /// 版本目录在、`node.exe` 不在 ⇒ `Missing`（"这一版没有可用的 node"），
    /// **不许**退回 `current` 或机器侧的 node。
    #[test]
    fn a_version_directory_without_node_exe_is_missing() {
        let temp = TempDir::new("shims-node-missing");
        let store = tuoen_store::Store::new(temp.path());
        std::fs::create_dir_all(temp.path().join(r"node\versions\24.19.0")).expect("建目录");
        assert_eq!(node_exe_for(&store, "v24.19.0"), NodeExe::Missing);

        // 版本号连拼路径都不合法 ⇒ 明确地报那一件事（而不是"没装"）。
        let unsafe_version = node_exe_for(&store, "vx/y");
        assert!(
            matches!(unsafe_version, NodeExe::UnsafeVersion(ref version) if version == "x/y"),
            "{unsafe_version:?}"
        );
    }

    /// 削前缀用的是检测那一层的**同一份**判据（`ToolSpec::strip_prefixes`），
    /// 而不是这里手写一个 `trim_start_matches('v')` —— 后者对 `v` 之外的形状会漂移。
    #[test]
    fn the_stripped_version_uses_the_detect_layer_prefix_rules() {
        let temp = TempDir::new("shims-node-quotes");
        let store = tuoen_store::Store::new(temp.path());
        temp.write(r"node\versions\24.19.0\node.exe", "MZ");
        // 带引号、带空白的输出也要能削对（`strip_prefixes` 先 trim 引号再削前缀）。
        for raw in ["v24.19.0", " v24.19.0 ", "\"v24.19.0\""] {
            assert!(
                matches!(node_exe_for(&store, raw), NodeExe::Found(_)),
                "{raw:?} 应当能削出 24.19.0"
            );
        }
        assert_eq!(
            node_exe_for(&store, "unknown"),
            NodeExe::Missing,
            "`unknown` 不是版本号 —— 一个目录都不许猜"
        );
    }
}
