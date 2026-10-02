//! 六个检测来源。
//!
//! 每个来源都是一个**纯查询**：输入 [`DetectContext`]（全部依赖已注入），输出
//! `Vec<DetectedTool>`。它们之间不互相调用 —— 合并与去重在 [`crate::detect::detect_all`]。
//!
//! **所有来源都只读。** 这个文件里没有写注册表、写文件、建链接、改环境变量的调用。

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use tuoen_platform::{EnvScope, RegHive};

use crate::detect::context::{DetectContext, PathEntry, PathScope, ScanRoot};
use crate::detect::spec::{self, ToolSpec};
use crate::detect::{Confidence, DetectedTool, DetectionSource};

// ─────────────────────────────────────────────────────────────────────────────
// 来源 1：tuoen 自己的安装记录
// ─────────────────────────────────────────────────────────────────────────────

/// `managed` 层：由 tuoen 自己安装的工具。
///
/// **L0 落地前这一层恒为空**，但判据本身必须存在 —— 否则七层置信度里少一个，
/// 而"我们自己装的"与"别人装的"在还原时的处理完全不同。
pub fn from_managed(ctx: &DetectContext<'_>) -> Vec<DetectedTool> {
    ctx.managed
        .installed()
        .into_iter()
        .filter_map(|record| {
            let spec = spec::spec_for_id(&record.name)?;
            Some(DetectedTool {
                name: spec.id.to_owned(),
                version: record.version,
                path: record.path,
                source: DetectionSource::Tuoen,
                confidence: Confidence::Managed,
                manager: None,
                evidence: "由 tuoen 自己安装（存在我们的安装记录）".to_owned(),
            })
        })
        .collect()
}

// ─────────────────────────────────────────────────────────────────────────────
// 来源 2：PATH 解析
// ─────────────────────────────────────────────────────────────────────────────

/// 把进程环境块里的 `Path` 拆成带来源层级的条目。
///
/// **来源层级的判定顺序**：进程块里有、注册表里也有 → 用注册表给的层级；
/// 只在进程块里有 → `process-only`。本机实测有 77 字符的 PowerShell MSIX 别名
/// **不在任何注册表里**，只读注册表的实现会漏掉它。
#[must_use]
pub fn path_entries(ctx: &DetectContext<'_>) -> Vec<PathEntry> {
    let machine: HashSet<String> = split_path(&ctx.env_var("Path").unwrap_or_default())
        .into_iter()
        .map(|s| normalize_for_compare(&s))
        .collect();
    let user_raw = ctx
        .env
        .get(EnvScope::User, "Path")
        .map(|v| v.value_expanded)
        .unwrap_or_default();
    let user: HashSet<String> = split_path(&user_raw)
        .into_iter()
        .map(|s| normalize_for_compare(&s))
        .collect();

    ctx.process_env
        .path_entries()
        .into_iter()
        .enumerate()
        .map(|(index, raw)| {
            let key = normalize_for_compare(&raw);
            // 用户级优先判：同名条目同时出现在两个 scope 时（本机的 `NVM_HOME` 就是
            // 双重管理腐坏的活症状），把用户级那一条算作用户级更符合"用户能改的是它"。
            let scope = if user.contains(&key) {
                PathScope::User
            } else if machine.contains(&key) {
                PathScope::Machine
            } else {
                PathScope::ProcessOnly
            };
            PathEntry { raw, scope, index }
        })
        .collect()
}

/// 按 `;` 拆 `PATH`，**保留空条目**（本机 HKLM `Path` 里真的有 `;;`，
/// 丢掉它就等于丢掉一个发现）。
#[must_use]
pub fn split_path(value: &str) -> Vec<String> {
    value.split(';').map(ToOwned::to_owned).collect()
}

fn normalize_for_compare(entry: &str) -> String {
    entry.trim().trim_end_matches(['\\', '/']).to_lowercase()
}

/// `PATH` 解析：对每个条目、每个已知可执行文件名做**定向探测**。
///
/// **为什么是定向探测而不是列目录**：本机 `PATH` 有 48 条，其中若干是
/// `C:\Windows\System32` 这种含数千文件的目录。逐条 `list_dir` 再逐个 `inspect`
/// 会让一次 `detect` 变成几万次系统调用。而"Windows 怎么找 `node.exe`"这件事本身就是
/// **按名字在目录里找**，所以定向探测不仅更快，它也更贴近真实语义。
///
/// **遮蔽检测是这里的副产品**：第一次见到某个文件名的那条 `PATH` 条目就是赢家，
/// 之后同名的都是"被遮蔽"。当赢家是机器级、而某个用户级条目也有同名文件时，
/// 我们（用户级 shim）永远轮不到执行 —— 这正是 ADR-0002 说的那个产品问题。
///
/// **只报告主命令。** 一次真机运行让这条变得必需：`node` 本来会报 4 行
/// （`node.exe` / `npm.cmd` / `npx.cmd` / `corepack.cmd`，版本分别是
/// `24.19.0` / `11.17.0` / `11.17.0` / `0.35.0`）—— 那是四个不同的版本号挤在
/// 同一个工具名下，用户没法回答"我的 Node 是哪个版本"。
/// 次要命令**仍然被探测**（shim 需要知道 `npm.cmd` 在哪），只是不进报告。
pub fn from_path(ctx: &DetectContext<'_>, entries: &[PathEntry]) -> Vec<DetectedTool> {
    let mut out = Vec::new();
    // 文件名 → 第一次解析到它的 PathEntry。**顺序即优先级**，所以只插一次。
    // 遮蔽是按**文件名**算的（Windows 就是这么找命令的），不是按工具。
    let mut first_seen: BTreeMap<String, PathEntry> = BTreeMap::new();

    for entry in entries {
        if entry.raw.trim().is_empty() {
            continue;
        }
        let dir = Path::new(entry.raw.trim());
        for spec in spec::KNOWN_TOOLS {
            for exe in spec.executables {
                let candidate = dir.join(exe.file);
                let facts = ctx.fs.inspect(&candidate);
                if !facts.exists || facts.is_dir {
                    continue;
                }

                // 遮蔽判定对**每个文件名**都要做，包括次要命令。
                let key = exe.file.to_lowercase();
                let winner_index = first_seen.get(&key).map(|first| first.index);
                if winner_index.is_none() {
                    first_seen.insert(key.clone(), entry.clone());
                }
                let is_first = winner_index.is_none();

                // 次要命令不进报告，但已经参与过遮蔽判定 —— 这正是我们想要的：
                // "`npm.cmd` 被机器级的那个遮蔽了"是一条真实且有用的发现。
                if !exe.primary {
                    continue;
                }

                // App Execution Alias：`exists == true`、`size == 0`、tag 0x8000001b。
                // **它排在 PATH 最前时是真凶**：本机 `python` 就是被它抢走的。
                let confidence = if facts.reparse.is_app_exec_alias() {
                    Confidence::AliasGhost
                } else {
                    Confidence::Executable
                };

                let version = if confidence == Confidence::Executable {
                    ctx.probe_version(spec, &candidate)
                } else {
                    // 对别名跑 `--version` 只会启动应用商店或弹窗，**不跑**。
                    None
                };

                let mut evidence = format!(
                    "PATH 第 {} 条（{}）里有 {}",
                    entry.index + 1,
                    entry.scope.as_str(),
                    exe.file
                );
                if confidence == Confidence::AliasGhost {
                    evidence.push_str("，但它是 0 字节的 App Execution Alias（tag 0x8000001b）");
                } else if version.is_none() {
                    evidence.push_str("，但没能问到版本");
                }
                if !is_first {
                    let winner = winner_index.map_or(0, |index| index + 1);
                    evidence.push_str(&format!("；**被 PATH 第 {winner} 条遮蔽**"));
                }

                out.push(DetectedTool {
                    name: spec.id.to_owned(),
                    version,
                    path: candidate.to_string_lossy().into_owned(),
                    source: DetectionSource::PathResolution,
                    confidence,
                    manager: None,
                    evidence,
                });
            }
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 来源 3：App Paths 注册表
// ─────────────────────────────────────────────────────────────────────────────

/// `App Paths` 注册表的子键位置（相对 hive 的 `SOFTWARE`）。
pub const APP_PATHS_SUBKEY: &str = r"Microsoft\Windows\CurrentVersion\App Paths";

/// **纯 `PATH` 扫描会整个漏掉这套查找机制。**
///
/// 本机实测：HKLM 42 条 + HKCU 19 条 = 61 条，而 Windows 的解析顺序是
/// **先 `PATH`、后 `App Paths`** —— 所以一条只在 App Paths 里的安装是"真的能敲，
/// 但不在 `PATH` 上"的，必须单独识别。
pub fn from_app_paths(ctx: &DetectContext<'_>) -> Vec<DetectedTool> {
    let mut out = Vec::new();
    let vars = ctx.process_vars();

    for hive in [RegHive::Hklm, RegHive::Hkcu] {
        let Ok(subkeys) = ctx.registry.subkeys(hive, APP_PATHS_SUBKEY) else {
            // 键不存在 = 没有东西，不是失败。
            continue;
        };
        for subkey in subkeys {
            let full = format!("{APP_PATHS_SUBKEY}\\{subkey}");
            // App Paths 的默认值就是可执行文件路径；有些条目还有 `Path` 值。
            let Some(raw) = ctx.registry.value(hive, &full, "") else {
                continue;
            };
            let Some(text) = raw.as_str() else {
                continue;
            };
            let expanded = tuoen_platform::expand_vars(text, &vars);

            let file_name = Path::new(&expanded)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| subkey.clone());
            let Some(spec) = spec::spec_for_file(&file_name) else {
                continue;
            };

            let facts = ctx.fs.inspect(Path::new(&expanded));
            let confidence = if !facts.exists {
                Confidence::RegisteredMissing
            } else if facts.reparse.is_app_exec_alias() {
                Confidence::AliasGhost
            } else {
                Confidence::Executable
            };

            let version = if confidence == Confidence::Executable {
                ctx.probe_version(spec, Path::new(&expanded))
            } else {
                None
            };

            out.push(DetectedTool {
                name: spec.id.to_owned(),
                version,
                path: expanded.clone(),
                source: DetectionSource::AppPaths,
                confidence,
                manager: None,
                evidence: format!(
                    "{} 的 App Paths 注册了 `{subkey}` → {expanded}{}",
                    hive.display_prefix(),
                    if facts.exists {
                        ""
                    } else {
                        "，但该文件不存在"
                    }
                ),
            });
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 来源 4：ARP 卸载键（三个 hive）
// ─────────────────────────────────────────────────────────────────────────────

/// ARP 卸载键的子键位置（相对 hive 的 `SOFTWARE`）。**三个 hive 用同一个写法。**
pub const ARP_SUBKEY: &str = r"Microsoft\Windows\CurrentVersion\Uninstall";

/// 三个 hive 的卸载键。**必须三个都读**：本机 HKCU 11 / HKLM 78 / WOW6432Node 30 ——
/// 漏掉 WOW6432Node 就漏掉一半的已安装软件。
///
/// **幽灵条目就是在这里被发现的**：本机 `Python311` 有 9 个活卸载键，而 `Test-Path`
/// 为 `False`。所以这里必须**真的去看文件在不在**，而不是相信注册表。
pub fn from_arp(ctx: &DetectContext<'_>) -> Vec<DetectedTool> {
    // 先用 `ArpEntry` 累积（它会把同一个工具同一版本的多个卸载键合并），
    // 最后一次性转成 `DetectedTool`。
    let mut out: Vec<ArpEntry> = Vec::new();
    let vars = ctx.process_vars();

    for hive in [RegHive::Hklm, RegHive::HklmWow6432, RegHive::Hkcu] {
        let Ok(subkeys) = ctx.registry.subkeys(hive, ARP_SUBKEY) else {
            continue;
        };
        for subkey in subkeys {
            let full = format!("{ARP_SUBKEY}\\{subkey}");
            let values = match ctx.registry.values(hive, &full) {
                Ok(values) => values,
                Err(_) => continue,
            };
            let get = |name: &str| {
                values
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(name))
                    .and_then(|(_, v)| v.as_str())
                    .map(ToOwned::to_owned)
            };

            let Some(display_name) = get("DisplayName") else {
                continue;
            };
            // 系统组件不是"开发工具"，跳过会让输出干净得多。
            if get("SystemComponent").is_some_and(|v| v.trim() == "1") {
                continue;
            }
            let Some(spec) = spec::spec_for_display_name(&display_name) else {
                continue;
            };

            let install_location = get("InstallLocation")
                .map(|text| tuoen_platform::expand_vars(&text, &vars))
                .filter(|text| !text.trim().is_empty())
                // 没有 `InstallLocation` 时问一次工具自己的注册位置。
                // **只对 CPython 这么做** —— 它有官方的注册表位置；别的工具没有，
                // 给它们编一个猜测等于制造假阳性。
                .or_else(|| {
                    (spec.id == "python")
                        .then(|| python_major_minor(&display_name))
                        .flatten()
                        .and_then(|version| python_install_path(ctx, &version))
                });

            // **关键判断：文件到底在不在。**
            let (path, exists) = match &install_location {
                Some(location) => {
                    let facts = ctx.fs.inspect(Path::new(location));
                    (location.clone(), facts.exists)
                }
                None => {
                    // 注册表既没给 InstallLocation、也没有工具自己的位置记录
                    // → **不猜就当幽灵**。
                    // 本机 Python 3.11.9 正是这种形状，而它是真的幽灵。
                    (String::new(), false)
                }
            };

            let confidence = if exists {
                // **不是 `Executable`。** `executable` 的判据是"在 `PATH` 上能解析到、
                // 文件真实存在且大小 > 0"，而 ARP 的 `InstallLocation` 是个**目录**、
                // 而且它不在 `PATH` 上。用 `registered` 说准确的事：
                // 注册表声称已装，目录真的在，我们不知道它是否可用。
                Confidence::Registered
            } else {
                Confidence::RegisteredMissing
            };

            // 只留下**安装器自己的**版本号，用于证据；真实版本在 `into_tool` 里问真实文件。
            let display_version = get("DisplayVersion");

            let mut display_names = vec![display_name.clone()];
            let mut subkeys_seen = vec![subkey.clone()];
            let mut hives_seen = vec![hive];

            if let Some(existing) = out.iter_mut().find(|entry: &&mut ArpEntry| {
                entry.spec_id == spec.id
                    && entry.confidence == confidence
                    && entry.path.eq_ignore_ascii_case(&path)
                    && entry.display_version == display_version
            }) {
                // 同一个工具、同一个版本、同一个路径，只是**另一个卸载键**。
                // 合并它们 —— 见 `ArpEntry` 的说明。
                existing.display_names.append(&mut display_names);
                existing.subkeys_seen.append(&mut subkeys_seen);
                existing.hives_seen.append(&mut hives_seen);
                continue;
            }

            out.push(ArpEntry {
                spec_id: spec.id,
                display_names,
                subkeys_seen,
                hives_seen,
                path,
                exists,
                confidence,
                display_version,
            });
        }
    }

    // 收尾：把累积的键与显示名写成人能读的证据。
    out.into_iter().map(|entry| entry.into_tool(ctx)).collect()
}

/// CPython 官方安装器注册的安装路径所在的位置。
///
/// `HKCU\SOFTWARE\Python\PythonCore\<版本>\InstallPath` 的默认值是安装根目录。
///
/// **这条存在的理由是"不许报假幽灵"**：本机 `Python 3.12.10 (64-bit)` 那个卸载键
/// **没有 `InstallLocation`**，所以只读 ARP 的实现会把它报成幽灵条目 ——
/// 而 Python 3.12 明明装得好好的。一个自己造的假幽灵比漏报更糟：
/// 用户会照着报告去"修"一个没坏的东西，然后不再信任这个工具。
///
/// **但它不能把真幽灵也救回来**：本机 `Python 3.11.9` 同样没有 `InstallLocation`，
/// 而它注册的路径 `...\Programs\Python\Python311\` **真的不存在** ——
/// 于是它仍然（正确地）是幽灵。两个案例的形状一模一样，只有文件系统能分开它们。
fn python_install_path(ctx: &DetectContext<'_>, version: &str) -> Option<String> {
    for hive in [RegHive::Hkcu, RegHive::Hklm, RegHive::HklmWow6432] {
        let subkey = format!(r"Python\PythonCore\{version}\InstallPath");
        let Some(value) = ctx.registry.value(hive, &subkey, "") else {
            continue;
        };
        let Some(text) = value.as_str() else {
            continue;
        };
        let expanded = tuoen_platform::expand_vars(text, &ctx.process_vars());
        if !expanded.trim().is_empty() {
            return Some(expanded);
        }
    }
    None
}

/// 从显示名里抠出 CPython 的 `X.Y` 版本（`Python 3.12.10 (64-bit)` → `3.12`）。
///
/// 官方安装器在注册表里用的是**两段**版本（`3.12`），不是显示名里的三段。
/// 抠不出来就返回 `None`，**不猜**。
fn python_major_minor(display_name: &str) -> Option<String> {
    let rest = display_name.split_once("Python ")?.1;
    let version = rest.split_whitespace().next()?;
    let mut parts = version.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.chars().all(|c| c.is_ascii_digit()) && minor.chars().all(|c| c.is_ascii_digit()) {
        Some(format!("{major}.{minor}"))
    } else {
        None
    }
}

/// 一个工具自己带的**二进制可能在的子目录**（相对安装根目录）。
///
/// 只列真实的形状：CPython 与 Temurin 用 `bin\`，Git for Windows 用 `cmd\`。
/// 不列就不猜 —— 猜一个不存在的目录只会多几次 `inspect`。
const BIN_SUBDIRS: &[&str] = &["", "bin", "cmd"];

/// 在安装根目录下找一个该工具的主可执行文件，并问它版本。
///
/// **为什么不直接用卸载键的 `DisplayVersion`**：本机实测它是**安装器的内部版本号**，
/// 不是工具版本：
/// - `Git_is1` 的 `DisplayVersion` 是 `2.53.0.0.7`，而 `git --version` 是 `2.53.0.windows.1`
/// - `Java 8 Update 491 (64-bit)` 是 `8.0.4910.10`，而 `java -version` 是 `1.8.0_491`
///
/// 把 `8.0.4910.10` 当成 Java 版本展示给用户是**错的**，而且它看起来像真版本号，
/// 所以比"未知"更糟。真实版本要问真实文件。
fn probe_installed_version(
    ctx: &DetectContext<'_>,
    spec: &'static ToolSpec,
    root: &str,
) -> Option<String> {
    let root = Path::new(root);
    for subdir in BIN_SUBDIRS {
        for exe in spec.executables {
            if !exe.primary {
                continue;
            }
            let candidate = if subdir.is_empty() {
                root.join(exe.file)
            } else {
                root.join(subdir).join(exe.file)
            };
            if !ctx.fs.inspect(&candidate).is_file() {
                continue;
            }
            if let Some(version) = ctx.probe_version(spec, &candidate) {
                return Some(version);
            }
        }
    }
    None
}

/// 一个 ARP 发现，**可能已经合并了多个卸载键**。
///
/// ## 为什么必须合并
///
/// 真机输出逼出来的：一台机器上 `python` 报了 **20 行**，其中 11 行是
/// `3.12.10150.0`、9 行是 `3.11.9150.0`，每行的路径都是"（无 InstallLocation）"。
/// 那是 20 个真实的卸载键（每个是 Python 的一个组件或一次修补安装的残留），
/// 但对用户来说它们是**两条**事实：3.11 是幽灵，3.12 是幽灵。
///
/// 二十行重复会把报告淹掉 —— 而报告被淹掉的后果是用户不再读它，
/// 那些真正重要的发现（`alias-ghost`、`manager-owned`）就跟着一起被忽略。
#[derive(Debug, Clone)]
struct ArpEntry {
    spec_id: &'static str,
    /// 全部合并进来的显示名（去重后进证据）。
    display_names: Vec<String>,
    /// 全部合并进来的卸载键。
    subkeys_seen: Vec<String>,
    hives_seen: Vec<tuoen_platform::RegHive>,
    path: String,
    exists: bool,
    confidence: Confidence,
    /// **安装器自己的**版本号（`DisplayVersion`）。
    ///
    /// **它不是工具版本**：本机实测 Git 是 `2.53.0.0.7`（真版本 `2.53.0.windows.1`）、
    /// Java 是 `8.0.4910.10`（真版本 `1.8.0_491`）。所以它只进证据、不进 `version` 字段。
    display_version: Option<String>,
}

impl ArpEntry {
    fn into_tool(self, ctx: &DetectContext<'_>) -> DetectedTool {
        let mut display_names = self.display_names;
        display_names.sort();
        display_names.dedup();
        let mut hives_seen = self.hives_seen;
        hives_seen.sort();
        hives_seen.dedup();
        let hives: Vec<&str> = hives_seen
            .iter()
            .map(|hive| hive.display_prefix())
            .collect();

        let name = display_names.first().cloned().unwrap_or_default();
        let mut evidence = if self.subkeys_seen.len() == 1 {
            format!(
                "{} 的卸载键 `{}` 报告已安装 `{name}`",
                hives.join("、"),
                self.subkeys_seen[0]
            )
        } else {
            format!(
                "{} 下的 {} 个卸载键都报告已安装 `{name}`（Python 的每个组件与每次修补都会留一个键）",
                hives.join("、"),
                self.subkeys_seen.len()
            )
        };

        if self.exists {
            evidence.push_str("，InstallLocation 存在");
        } else {
            evidence.push_str("，但它的 InstallLocation 不存在 —— **幽灵条目**");
        }
        if display_names.len() > 1 {
            evidence.push_str(&format!("；合并了 {} 个显示名", display_names.len()));
        }

        // 合并之后的路径。
        //
        // **`path` 字段必须是一条路径或一个明确的占位，绝不能是一句话。**
        // 独立验收抓到了这一点：没有 `InstallLocation` 时这里曾经是
        // `（无 InstallLocation；卸载键 {…}）` —— 一句中文说明出现在一个叫 `path`
        // 的字段里，任何按路径解析它的消费者都会失败。现在写成明确的占位符，
        // 说明留在 `evidence` 里。
        //
        // **版本要在搬走 `self.path` 之前问真实文件** —— 用真实路径而不是占位符：
        // 占位符不是路径，拿它去 `inspect` 只会白跑几次系统调用。
        let spec = spec::spec_for_id(self.spec_id);
        let probed = if self.confidence.is_usable() && !self.path.is_empty() {
            spec.and_then(|spec| probe_installed_version(ctx, spec, &self.path))
        } else {
            None
        };

        let path = if self.path.is_empty() {
            format!("<无 InstallLocation，卸载键 {}>", self.subkeys_seen[0])
        } else {
            self.path
        };

        // 安装器版本与真实版本不一致时**两个都写出来** —— 那是一条真实发现：
        // "卸载键说 8.0.4910.10，实际是 1.8.0_491"正是让人困惑的东西。
        match (&probed, &self.display_version) {
            (Some(real), Some(installer)) if real != installer => {
                evidence.push_str(&format!(
                    "；安装器登记的版本是 `{installer}`，实际是 `{real}`"
                ));
            }
            (None, Some(installer)) => {
                evidence.push_str(&format!(
                    "；安装器登记的版本是 `{installer}`（未能问到真实版本）"
                ));
            }
            _ => {}
        }

        DetectedTool {
            name: self.spec_id.to_owned(),
            version: probed,
            path,
            source: DetectionSource::RegistryArp,
            confidence: self.confidence,
            manager: None,
            evidence,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// 来源 5：文件系统扫描
// ─────────────────────────────────────────────────────────────────────────────

/// 扫已知的开发根目录，找**任何注册表都没提到的**工具安装。
///
/// 本机实例：`C:\Dev\Tool\apache-maven-3.9.5`（1,408.8 MB）既不在 winget 也不在
/// `PATH`，但 `~/.m2/repository`（461.6 MB）证明它在用。**这类东西是最容易在换电脑时丢掉的。**
pub fn from_filesystem_scan(ctx: &DetectContext<'_>) -> Vec<DetectedTool> {
    let mut out = Vec::new();
    let mut seen_paths: BTreeSet<String> = BTreeSet::new();

    for ScanRoot { path, why } in &ctx.scan_roots {
        if !ctx.fs.inspect(path).exists {
            continue;
        }
        for entry in ctx.fs.list_dir(path) {
            if !entry.is_dir {
                continue;
            }
            let child = path.join(&entry.name);
            let Some(spec) = spec::spec_for_display_name(&entry.name) else {
                continue;
            };
            let key = child.to_string_lossy().to_lowercase();
            if !seen_paths.insert(key) {
                continue;
            }
            let version = spec::version_from_path_hint(&entry.name);
            out.push(DetectedTool {
                name: spec.id.to_owned(),
                version,
                path: child.to_string_lossy().into_owned(),
                source: DetectionSource::FilesystemScan,
                confidence: Confidence::DirectoryOnly,
                manager: None,
                evidence: format!("{why}：`{}` 看起来是一个安装目录", entry.name),
            });
        }
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 来源 6：第三方版本管理器
// ─────────────────────────────────────────────────────────────────────────────

/// 一个已知的第三方版本管理器。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManagerSpec {
    /// 管理器 id（`nvm4w` / `uv` / `pyenv-win` / `mise` / `asdf`）。
    pub id: &'static str,
    /// 它管的是哪个逻辑工具。
    pub tool: &'static str,
    /// 识别它的环境变量名。
    pub env_vars: &'static [&'static str],
    /// 识别它的目录（存在即认为装了）。
    pub probe_dirs: &'static [&'static str],
}

/// 已知版本管理器表。
///
/// **本机活证据**：`NVM_HOME` 与 `NVM_SYMLINK` **同时存在于 `HKCU\Environment`
/// 和 `HKLM\Session Manager\Environment`** —— 这是真实的双重管理腐坏症状，
/// 也是"采纳而不是接管"（ADR-0004）的证据来源。
pub const KNOWN_MANAGERS: &[ManagerSpec] = &[
    ManagerSpec {
        id: "nvm4w",
        tool: "node",
        env_vars: &["NVM_HOME", "NVM_SYMLINK"],
        probe_dirs: &[],
    },
    ManagerSpec {
        id: "uv",
        tool: "python",
        env_vars: &["UV_PYTHON"],
        probe_dirs: &[],
    },
    ManagerSpec {
        id: "pyenv-win",
        tool: "python",
        env_vars: &["PYENV", "PYENV_ROOT"],
        probe_dirs: &[],
    },
    ManagerSpec {
        id: "mise",
        tool: "node",
        env_vars: &["MISE_DATA_DIR"],
        probe_dirs: &[],
    },
    ManagerSpec {
        id: "asdf",
        tool: "node",
        env_vars: &["ASDF_DATA_DIR"],
        probe_dirs: &[],
    },
];

/// 识别第三方版本管理器，并列出它管理的工具。
///
/// **只读采纳**（ADR-0004）：能看见、能选中、能 pin、能使用；但 `uninstall` 不会去动它们。
/// 接管 nvm4w 意味着改写它那个**需要管理员才能重建**的符号链接，
/// 且两个版本管理器会争抢同一个 reparse point。
pub fn from_managers(ctx: &DetectContext<'_>) -> Vec<DetectedTool> {
    let mut out = Vec::new();

    for manager in KNOWN_MANAGERS {
        // 环境变量：用户级与机器级都要看 —— 本机 NVM_* 在两个 scope 里都有。
        let mut hits: Vec<(String, String, PathScope)> = Vec::new();
        for name in manager.env_vars {
            for scope in [EnvScope::User, EnvScope::Machine] {
                if let Some(var) = ctx.env.get(scope, name) {
                    let scope = if scope == EnvScope::User {
                        PathScope::User
                    } else {
                        PathScope::Machine
                    };
                    hits.push((name.to_string(), var.value_expanded, scope));
                }
            }
        }
        if hits.is_empty() {
            continue;
        }

        // 管理器表里的 `tool` 必须是我们知道的工具；`KNOWN_MANAGERS` 的测试钉住了这条，
        // 所以这里不再重复查一遍（查了也只是为了断言，反而会留下一个未使用的绑定）。
        if spec::spec_for_id(manager.tool).is_none() {
            continue;
        }

        // 双重 scope 是必须报告的信号，不是噪声。
        let mut scopes: Vec<PathScope> = hits.iter().map(|(_, _, s)| *s).collect();
        scopes.sort();
        scopes.dedup();
        let duplicated = scopes.len() > 1;

        // 报告里用的路径：优先 NVM_HOME 这类"版本库根目录"，否则第一个命中。
        let path = hits
            .iter()
            .find(|(name, _, _)| name.ends_with("_HOME") || name.ends_with("_ROOT"))
            .or_else(|| hits.first())
            .map(|(_, value, _)| value.clone())
            .unwrap_or_default();

        let mut evidence = format!(
            "环境变量 {} 说明这台机器由 `{}` 管理 {}",
            hits.iter()
                .map(|(name, value, scope)| format!("{name}={value}（{}）", scope.as_str()))
                .collect::<Vec<_>>()
                .join("、"),
            manager.id,
            manager.tool
        );
        if duplicated {
            evidence.push_str("；**同名变量同时存在于用户级与机器级** —— 双重管理腐坏");
        }

        // 该管理器当前生效的版本目录（nvm4w 的符号链接指向它）。
        let version = hits
            .iter()
            .find(|(name, _, _)| name == "NVM_SYMLINK")
            .and_then(|(_, value, _)| {
                let facts = ctx.fs.inspect(Path::new(value));
                facts.link_target.clone()
            })
            .and_then(|target| spec::version_from_path_hint(&target));

        out.push(DetectedTool {
            name: manager.tool.to_owned(),
            version,
            path,
            source: DetectionSource::Manager,
            confidence: Confidence::ManagerOwned,
            manager: Some(manager.id.to_owned()),
            evidence,
        });
    }

    out
}

// ─────────────────────────────────────────────────────────────────────────────
// 合并
// ─────────────────────────────────────────────────────────────────────────────

/// 六个来源的固定顺序。**`--json` 必须逐字节稳定**（决策 35），所以顺序是契约。
pub const SOURCE_ORDER: &[DetectionSource] = &[
    DetectionSource::Tuoen,
    DetectionSource::PathResolution,
    DetectionSource::AppPaths,
    DetectionSource::RegistryArp,
    DetectionSource::FilesystemScan,
    DetectionSource::Manager,
];

/// 合并六个来源的结果。
///
/// **只去重完全相同的 (name, path, source) 三元组** —— 不按名字合并。
/// 理由：同一个逻辑工具被多个机制管理**正是我们要报告的东西**
/// （`tool.multi-manager` 检查项的存在理由）。按名字合并会把最重要的信号吃掉。
#[must_use]
pub fn merge(mut groups: Vec<Vec<DetectedTool>>) -> Vec<DetectedTool> {
    let mut seen: HashSet<(String, String, DetectionSource)> = HashSet::new();
    let mut out = Vec::new();

    // 按 `SOURCE_ORDER` 的次序取组；多出来的组（未来新增来源）按传入顺序排在后面。
    let order: Vec<DetectionSource> = SOURCE_ORDER.to_vec();
    groups.sort_by_key(|group| {
        group
            .first()
            .and_then(|tool| order.iter().position(|s| *s == tool.source))
            .unwrap_or(usize::MAX)
    });

    for group in groups {
        for tool in group {
            let key = (tool.name.clone(), tool.path.to_lowercase(), tool.source);
            if seen.insert(key) {
                out.push(tool);
            }
        }
    }

    // 组内与组间都排序：先按工具名（表顺序），再按路径。
    // 用"已知工具表里的序号"排序，这样输出顺序与 `KNOWN_TOOLS` 一致、稳定可预测。
    out.sort_by(|a, b| {
        let ai = spec::spec_for_id(&a.name).map_or(usize::MAX, |s| {
            spec::KNOWN_TOOLS
                .iter()
                .position(|k| k.id == s.id)
                .unwrap_or(usize::MAX)
        });
        let bi = spec::spec_for_id(&b.name).map_or(usize::MAX, |s| {
            spec::KNOWN_TOOLS
                .iter()
                .position(|k| k.id == s.id)
                .unwrap_or(usize::MAX)
        });
        ai.cmp(&bi)
            .then_with(|| a.path.to_lowercase().cmp(&b.path.to_lowercase()))
            .then_with(|| a.source.as_str().cmp(b.source.as_str()))
    });

    out
}

/// 一个工具的规格（给上层做展示用）。
#[must_use]
pub fn spec_of(tool: &DetectedTool) -> Option<&'static ToolSpec> {
    spec::spec_for_id(&tool.name)
}

/// 默认的扫描根目录（本机取证得出的形状）。
///
/// **不做全盘扫描**：那会花几分钟并且读到用户根本不关心的目录。
/// 这些是"开发工具实际住的地方"—— 本机 `C:\Dev` 14.3 GB、`C:\Work` 9.1 GB。
#[must_use]
pub fn default_scan_roots(env: &dyn tuoen_platform::ProcessEnv) -> Vec<ScanRoot> {
    let mut roots = Vec::new();
    let mut push = |path: PathBuf, why: &'static str| {
        roots.push(ScanRoot { path, why });
    };

    // 用户目录下的常见位置。
    if let Some(profile) = env
        .vars()
        .into_iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("USERPROFILE"))
        .map(|(_, v)| v)
    {
        let profile = PathBuf::from(profile);
        push(profile.join(".local").join("bin"), "用户级工具目录");
        push(
            profile.join("scoop").join("apps"),
            "Scoop 的安装目录（只读发现）",
        );
        push(profile.join(".cargo").join("bin"), "Rust 工具链的 bin");
    }

    // 常见的"我自己放工具的地方"（本机 C:\Dev 14.3 GB 就是这种）。
    for (path, why) in [
        (r"C:\Dev", "本机实测的开发根目录（14.3 GB）"),
        (r"C:\Dev\Tool", "本机实测的手工解压工具目录（Maven 在这里）"),
        (r"C:\Dev\base", "本机实测的基础工具目录（JDK 在这里）"),
        (r"C:\Tools", "常见的手工工具目录"),
        (r"C:\Software", "常见的手工工具目录"),
    ] {
        push(PathBuf::from(path), why);
    }

    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_order_is_the_json_contract() {
        // 改动这个顺序会改变 --json 的输出顺序 = 破坏性变更。
        assert_eq!(SOURCE_ORDER[0], DetectionSource::Tuoen);
        assert_eq!(SOURCE_ORDER[1], DetectionSource::PathResolution);
        assert_eq!(SOURCE_ORDER.len(), 6);
    }

    #[test]
    fn split_path_keeps_empty_entries() {
        // 本机 HKLM Path 里真的有 `;;`，丢掉它就等于丢掉一个发现。
        assert_eq!(
            split_path(r"C:\Windows;;C:\Tools"),
            vec![
                r"C:\Windows".to_owned(),
                String::new(),
                r"C:\Tools".to_owned()
            ]
        );
    }

    #[test]
    fn compare_normalisation_ignores_trailing_separator_and_case() {
        assert_eq!(
            normalize_for_compare(r"C:\Windows\"),
            normalize_for_compare(r"c:\windows")
        );
    }

    #[test]
    fn merge_does_not_collapse_the_same_tool_from_two_sources() {
        // 同一个逻辑工具被两个机制管理**正是要报告的**，不是要合并掉的。
        let a = DetectedTool {
            name: "node".to_owned(),
            version: Some("24.19.0".to_owned()),
            path: r"C:\a".to_owned(),
            source: DetectionSource::PathResolution,
            confidence: Confidence::Executable,
            manager: None,
            evidence: "x".to_owned(),
        };
        let b = DetectedTool {
            source: DetectionSource::Manager,
            confidence: Confidence::ManagerOwned,
            manager: Some("nvm4w".to_owned()),
            ..a.clone()
        };
        let merged = merge(vec![vec![a.clone()], vec![b.clone()]]);
        assert_eq!(merged.len(), 2, "两个来源的记录必须都留下");
    }

    #[test]
    fn merge_drops_only_exact_triples() {
        let a = DetectedTool {
            name: "node".to_owned(),
            version: None,
            path: r"C:\a".to_owned(),
            source: DetectionSource::PathResolution,
            confidence: Confidence::Executable,
            manager: None,
            evidence: "x".to_owned(),
        };
        let merged = merge(vec![vec![a.clone()], vec![a.clone()]]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn merge_sorts_by_known_tool_order_then_path() {
        let make = |name: &str, path: &str| DetectedTool {
            name: name.to_owned(),
            version: None,
            path: path.to_owned(),
            source: DetectionSource::PathResolution,
            confidence: Confidence::Executable,
            manager: None,
            evidence: String::new(),
        };
        let merged = merge(vec![vec![
            make("git", r"C:\z"),
            make("node", r"C:\b"),
            make("node", r"C:\a"),
        ]]);
        let order: Vec<(&str, &str)> = merged
            .iter()
            .map(|t| (t.name.as_str(), t.path.as_str()))
            .collect();
        // node 在表里排在 git 之前（KNOWN_TOOLS 的顺序），同工具内按路径排。
        assert_eq!(
            order,
            vec![("node", r"C:\a"), ("node", r"C:\b"), ("git", r"C:\z")]
        );
    }

    #[test]
    fn manager_table_has_no_duplicate_ids() {
        let mut seen = std::collections::HashSet::new();
        for manager in KNOWN_MANAGERS {
            assert!(seen.insert(manager.id), "{} 重复", manager.id);
            assert!(
                spec::spec_for_id(manager.tool).is_some(),
                "{} 管着一个我们不知道的工具 {}",
                manager.id,
                manager.tool
            );
            assert!(
                !manager.env_vars.is_empty(),
                "{} 没有任何识别依据 —— 那它就永远不会被发现",
                manager.id
            );
        }
    }

    #[test]
    fn app_paths_and_arp_subkeys_are_the_real_ones() {
        assert_eq!(
            APP_PATHS_SUBKEY,
            r"Microsoft\Windows\CurrentVersion\App Paths"
        );
        assert_eq!(ARP_SUBKEY, r"Microsoft\Windows\CurrentVersion\Uninstall");
    }
}
