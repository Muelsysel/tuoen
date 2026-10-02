//! `tool.*` 检查 —— 工具的**管理机制**、`PATH` 解析、全局包前缀与没人管的目录。
//!
//! 输入是四列事实：`tools.toml`（[`MachineFacts::tools`]）、每条命令解析到了哪个目录
//! （[`MachineFacts::resolution`]）、第三方管理器的全局包前缀
//! （[`MachineFacts::global_prefix`]）、扫描根下面那些"看起来是开发工具"的目录
//! （[`MachineFacts::dev_roots`]）。**一次也不去问机器。**
//!
//! | ID | 严重度 | 判据 | 真机实例 |
//! |---|---|---|---|
//! | `tool.multi-manager` | **error** | 同一个逻辑工具出现在两个以上**机制**里 | node 被 nvm4w 管、被我们管、还能在 `PATH` 上解析到 |
//! | `tool.multiple-active` | **error** | 同一工具的不同命令落在两个以上目录 | `java` → Oracle `java8path`，`javac` → `C:\Dev\base\JDK\JDK8\bin` |
//! | `tool.ghost` | warn | `confidence = registered-missing` | `Python311`（9 个卸载键、2 条 `PATH`、`Test-Path` 为假） |
//! | `tool.global-prefix-inside-version-dir` | **error** | 全局包前缀是 reparse point，或路径里有版本成分 | `npm config get prefix` = `C:\nvm4w\nodejs`（符号链接） |
//! | `tool.unmanaged-directory` | info | 没有任何机制在管它（`managed_by` 为空） | `C:\Dev\Tool\apache-maven-3.9.5`（1.4 GB，`~/.m2/repository` 461 MB 证明在用）、`C:\Dev\base\JDK` |
//!
//! 每条检查旁边写着**反例为什么健康** —— 票据的硬性要求是"每个正例都要有反例"，
//! 否则检查项会变成永远报错的噪声源。

use std::collections::{BTreeMap, BTreeSet};

use super::super::facts::PrefixOrigin;
use super::super::{
    CommandResolution, DevRoot, Finding, GlobalPrefix, MachineFacts, Severity, ids, sources,
};
use crate::capture::{ToolRow, ToolsFile};
use crate::detect::Confidence;

/// 跑这一族的检查。
///
/// 族内顺序固定（`diagnose` 之后还会按「严重度 + ID」整体排一遍），所以两次 `doctor`
/// 的输出逐字节相同这条性质不依赖调用顺序。
#[must_use]
pub fn run(facts: &MachineFacts) -> Vec<Finding> {
    let mut findings = Vec::new();
    findings.extend(multi_manager(&facts.tools));
    findings.extend(multiple_active(&facts.resolution, &facts.tools));
    findings.extend(ghosts(&facts.tools));
    findings.extend(prefixes_inside_version_dirs(&facts.global_prefix));
    findings.extend(unmanaged_directories(&facts.dev_roots));
    findings
}

/// 七档置信度**从弱到强**。
///
/// # 为什么不直接用 `Confidence` 的 `Ord`
///
/// `Confidence` 的声明顺序是"给人看的分组顺序"（`Managed` 排在最前），而这里要的是
/// "这一批行里**最不可信**的那一档"。两者是不同的次序：照抄枚举顺序会得出
/// "`managed` 最弱"这种反话。
///
/// # 这个次序的判据
///
/// 前三档是"看起来装了、其实用不了"（[`Confidence::is_usable`] 为假），按
/// "有多没用"排：`registered-missing`（根本不在）< `alias-ghost`（0 字节别名）
/// < `directory-only`（有个目录，不知道谁装的）。后四档是能用的那四档，按
/// "我们知道多少"排：只有注册表一条目录记录（`registered`）< 别人的版本管理器
/// 在管（`manager-owned`）< `PATH` 上真能跑（`executable`）< 我们自己装的、
/// 能重建（`managed`）。
///
/// 用例 `the_weakness_table_covers_every_confidence_level_exactly_once` 钉住它与
/// 检测引擎的七个 slug 一一对应，改名会红。
const WEAKEST_TO_STRONGEST: &[&str] = &[
    "registered-missing",
    "alias-ghost",
    "directory-only",
    "registered",
    "manager-owned",
    "executable",
    "managed",
];

/// 档位在 [`WEAKEST_TO_STRONGEST`] 里的位置（越小越弱）。认不出来的排到最后。
fn rank(level: &str) -> usize {
    WEAKEST_TO_STRONGEST
        .iter()
        .position(|slug| *slug == level)
        .unwrap_or(usize::MAX)
}

/// 这批行里**最弱**的那一档，用来填 `Finding::confidence`。
///
/// 认不出来的 slug 直接跳过：`Finding::confidence` 是 `&'static str`，一个我们没有
/// 档位的字符串变不成它。全都不认识时返回 `None` —— 那是"这组结论没有检测引擎的
/// 档位"，比编一个更诚实。
fn weakest_confidence<'a>(rows: impl IntoIterator<Item = &'a ToolRow>) -> Option<&'static str> {
    let mut weakest: Option<&'static str> = None;
    for row in rows {
        let Some(level) = WEAKEST_TO_STRONGEST
            .iter()
            .find(|slug| **slug == row.confidence)
            .copied()
        else {
            continue;
        };
        weakest = Some(match weakest {
            Some(current) if rank(current) < rank(level) => current,
            _ => level,
        });
    }
    weakest
}

/// `tool.multi-manager`（**error**）：同一个逻辑工具被**两个以上版本管理器**管着。
///
/// # 判据是"有几个**管理器**"，不是"有几个来源"
///
/// **只有 `manager`（带名字）与 `tuoen` 算管理器。** `path-resolution` / `app-paths` /
/// `registry-arp` / `filesystem-scan` 都只是"我们知道它在哪"：卸载键是**登记**，
/// `PATH` 是**查找顺序**，扫描是**看了一眼** —— 它们都不负责把东西装回来，
/// 也不负责切版本。
///
/// 第一版按"来源种类 ≥ 2"判，于是真机上 `dotnet` / `git` / `java` / `node` /
/// `python` / `wsl` **六条全被报成 `error`**，而"既在 `PATH` 上又在卸载键里"
/// 是**完全正常**的 —— 六条里五条是假的。这正是票据反复提醒的那种失败：
/// **一条永远报错的检查会训练用户忽略所有发现**（决策 102）。
///
/// # 严重度为什么是 error
///
/// **静默失效**：用户切了 nvm4w 的版本，我们那份（`tuoen` 管的）纹丝不动，
/// 而两条记录在报告里都像是"当前的"。本机 node 就是这样（nvm4w + tuoen）。
///
/// # 置信度取"最弱的那一档"
///
/// 这条结论的硬度由**最弱**的那一行决定。本机 node 的三行是 `manager-owned` /
/// `managed` / `executable`，报出来的就是 `manager-owned`。
///
/// # 来源为什么填 `manager`
///
/// 这条结论来自检测引擎里那些**管理器**行，而 `Finding::source` 只能填一个 slug。
///
/// # 反例为什么健康
///
/// ① 一个工具只被一个管理器管（哪怕它有五行别的来源）；② 一个工具没有任何管理器
/// （手工解压的 Maven 就是这一档，那是 `tool.unmanaged-directory` 的事）。
fn multi_manager(tools: &ToolsFile) -> Vec<Finding> {
    let mut by_tool: BTreeMap<String, Vec<&ToolRow>> = BTreeMap::new();
    for row in &tools.tool {
        by_tool
            .entry(row.name.to_lowercase())
            .or_default()
            .push(row);
    }

    by_tool
        .into_values()
        .filter_map(|rows| {
            let name = rows.first()?.name.clone();
            let managers: BTreeSet<String> = rows
                .iter()
                .filter_map(|row| match row.source.as_str() {
                    "manager" => Some(
                        row.manager
                            .clone()
                            .unwrap_or_else(|| "unknown-manager".to_owned()),
                    ),
                    "tuoen" => Some("tuoen".to_owned()),
                    _ => None,
                })
                .collect();
            if managers.len() < 2 {
                return None;
            }
            // 一个管理器一行。`manager=nvm4w` / `manager=tuoen` 才是用户能去动的东西。
            // 顺序由 `BTreeSet` 决定（字典序），所以两次运行的输出逐字节相同。
            let evidence: Vec<String> = managers
                .iter()
                .map(|manager| format!("{name} manager={manager}"))
                .collect();
            let mut finding = Finding::new(
                ids::TOOL_MULTI_MANAGER,
                Severity::Error,
                format!(
                    "工具 {name} 同时被多个版本管理器管着（{}）—— 每个管理器各管一份：\
                     在其中一边切换版本不会动另一边，而报告里每一条都像是「当前的」",
                    managers.iter().cloned().collect::<Vec<_>>().join(" / ")
                ),
                evidence,
                sources::MANAGER,
            );
            if let Some(confidence) = weakest_confidence(rows.iter().copied()) {
                finding = finding.with_confidence(confidence);
            }
            Some(finding)
        })
        .collect()
}

/// 一个工具的命令都解析到了哪些目录。
#[derive(Debug)]
struct ToolDirs {
    /// 工具 id 的原样写法（先出现的那一个）。
    tool_id: String,
    /// 目录 → 第一个落在那里的命令行（顺序 = `resolution` 的顺序）。
    dirs: Vec<(String, String)>,
}

/// `tool.multiple-active`（**error**）：同一个工具的不同命令**落在两个以上目录**。
///
/// # 判据是目录不同，版本号不是证据
///
/// 本机 `java` 在 `…\Common Files\Oracle\Java\java8path`，而 `javac` / `jar` 在
/// `C:\Dev\base\JDK\JDK8\bin` —— 两个安装，两个厂商。而 `AGENTS.md` 里那条教训是：
/// 判定"谁赢了名字冲突"只能看解析结果，**版本号不是证据**（本机两个 Node 的版本号
/// 恰好相同，"版本对上了"是假象）。所以这里只比目录，连 `version` 都不读。
///
/// # 为什么 `python` 也会响，而且不该被过滤掉
///
/// 本机 `python` 解析到 `…\Microsoft\WindowsApps`（0 字节的 App Execution Alias），
/// `pip` 解析到 `…\Programs\Python\Python312\Scripts`。这**是真的**：
/// `pip` 装的东西与 `python` 跑的东西不是同一套。为了"少报几条"把它过滤掉，
/// 正是这条检查最该避免的事（用例 `the_windowsapps_alias_directory_still_counts_...`
/// 钉的就是它不许被过滤）。
///
/// # 置信度
///
/// 这条结论来自 `PATH` 解析，所以取该工具**那批 `path-resolution` 行里最弱的
/// 一档**；一条这样的行都没有时，退回到该工具全部行里最弱的一档。
///
/// # 反例为什么健康
///
/// 所有命令都落在同一个目录（哪怕目录名拼写大小写不同、结尾多个反斜杠），
/// 或者有的命令根本不在 `PATH` 上 —— 两者都不是"另一套安装"的证据。
fn multiple_active(resolution: &[CommandResolution], tools: &ToolsFile) -> Vec<Finding> {
    let mut by_tool: BTreeMap<String, ToolDirs> = BTreeMap::new();
    for row in resolution {
        let Some(dir) = row.directory.as_deref() else {
            // `PATH` 上没有这条命令 —— 它不是"另一套安装"的证据。
            continue;
        };
        let entry = by_tool
            .entry(row.tool_id.to_lowercase())
            .or_insert_with(|| ToolDirs {
                tool_id: row.tool_id.clone(),
                dirs: Vec::new(),
            });
        if !entry.dirs.iter().any(|(known, _)| same_dir(known, dir)) {
            entry.dirs.push((dir.to_owned(), row.command.clone()));
        }
    }

    by_tool
        .into_values()
        .filter(|entry| entry.dirs.len() >= 2)
        .map(|entry| {
            let evidence: Vec<String> = entry
                .dirs
                .iter()
                .map(|(dir, command)| {
                    format!("tool={} command={} dir={}", entry.tool_id, command, dir)
                })
                .collect();
            let name_matches = |row: &&ToolRow| row.name.eq_ignore_ascii_case(&entry.tool_id);
            let confidence = weakest_confidence(
                tools
                    .tool
                    .iter()
                    .filter(|row| row.source == "path-resolution")
                    .filter(name_matches),
            )
            .or_else(|| weakest_confidence(tools.tool.iter().filter(name_matches)));
            let mut finding = Finding::new(
                ids::TOOL_MULTIPLE_ACTIVE,
                Severity::Error,
                format!(
                    "工具 {} 的不同命令解析到了 {} 个不同的目录 ——「同一个工具」是假象：不同子命令来自不同安装，行为会随命令变（版本号不是证据）",
                    entry.tool_id,
                    entry.dirs.len()
                ),
                evidence,
                sources::PATH_RESOLUTION,
            );
            if let Some(confidence) = confidence {
                finding = finding.with_confidence(confidence);
            }
            finding
        })
        .collect()
}

/// 两条目录文本是不是同一个目录。Windows 的路径比较忽略大小写与尾部反斜杠 ——
/// 不归一化的话，`C:\Tools` 与 `c:\tools\` 会被报成"两套安装"。
fn same_dir(a: &str, b: &str) -> bool {
    a.trim()
        .trim_matches('"')
        .trim_end_matches('\\')
        .to_lowercase()
        == b.trim().trim_end_matches('\\').to_lowercase()
}

/// 一条幽灵记录的路径，**ASCII 化**之后才进 `evidence`。
///
/// # 为什么不能原样印
///
/// `--json` 的成功载荷里**不许有 CJK**（CLI 的契约用例逐字断言这件事），而引擎在
/// 答不出位置时写的是**占位符**：`<无 InstallLocation，卸载键 {GUID}>`
/// （见 `detect/engine.rs` —— `path` 字段只能是路径或明确的占位，不能是一句话）。
/// 占位符里带中文，原样印进 `evidence` 会把整份 `--json` 弄脏；真机上 `tool.ghost`
/// 的四条里有三条走的正是这条路。
///
/// # 但卸载键不能丢
///
/// 没有 `InstallLocation` 的幽灵条目**只有那个卸载键能定位它** —— 丢了它，
/// 用户拿到的是"有个幽灵，但你永远找不到它"。所以把 `{}` 里那一段抽出来单独作为
/// `uninstall-key=`。抽的是花括号，不是在匹配中文：判据不依赖占位符的措辞。
///
/// 两条规则同一句话：宁可少说，也不要说出一句没法被脚本匹配的话。
#[must_use]
fn ascii_evidence_path(path: &str) -> (String, Option<String>) {
    if path.is_ascii() {
        return (path.to_owned(), None);
    }
    let key = path
        .split_once('{')
        .and_then(|(_, rest)| rest.split_once('}'))
        .map(|(inner, _)| format!("{{{inner}}}"))
        // 花括号里还必须全是 ASCII：里面要还有中文，那它就不是一个注册表键，
        // 宁可不要它也不要弄脏 `--json`。
        .filter(|key| key.is_ascii());
    let label = if path.starts_with('<') && path.ends_with('>') {
        // 引擎自己的占位符：它明确说了"这不是一条路径"。
        "<placeholder>"
    } else {
        // 一条真的带非 ASCII 字符的路径（中文用户名、中文目录名）。
        "<non-ascii>"
    };
    (label.to_owned(), key)
}

/// `tool.ghost`（warn）：注册表声称装了，而记录里的路径**不在**
/// （`confidence = registered-missing`）。
///
/// # 只报告，绝不删
///
/// 票据原话：**不产生任何删除动作**。这条检查只读事实，而 [`crate::doctor::diagnose`]
/// 是纯函数（不 spawn、不读注册表、不写任何东西），所以"删掉幽灵条目"在这里连一个
/// 能落脚的地方都没有。用例 `a_registry_entry_whose_files_are_gone_is_a_ghost`
/// 跑完前后把假注册表与假文件系统逐项比一遍，钉的就是这件事 —— **声称没碰 ≠ 证明没碰**。
///
/// # 一条 finding 一个工具
///
/// 真机上 `python` 有两行 `registered-missing`（一个卸载键 + 一条 `PATH` 条目）、
/// `dotnet` 两行、`java` / `wsl` 各一行 —— 合并成四条才读得下去；`evidence` 里把
/// 该工具**全部**幽灵路径列出来（位置答不出来时按 [`ascii_evidence_path`] 写
/// `<placeholder>` + `uninstall-key=`）。
///
/// # 真机实例
///
/// `Python311`：9 个卸载键、2 条 `PATH`，而安装目录不存在。
///
/// # 反例为什么健康
///
/// `registered`（目录真的在）与 `executable`（`PATH` 上真能跑）不是幽灵：
/// 注册表的说法与磁盘一致。判据只认 `registered-missing` 这一档。
fn ghosts(tools: &ToolsFile) -> Vec<Finding> {
    let mut by_tool: BTreeMap<String, Vec<&ToolRow>> = BTreeMap::new();
    for row in &tools.tool {
        if row.confidence == Confidence::RegisteredMissing.as_str() {
            by_tool
                .entry(row.name.to_lowercase())
                .or_default()
                .push(row);
        }
    }

    by_tool
        .into_values()
        .filter_map(|rows| {
            let name = rows.first()?.name.clone();
            let evidence: Vec<String> = rows
                .iter()
                .flat_map(|row| {
                    let (path, key) = ascii_evidence_path(&row.path);
                    let mut lines = vec![format!("tool={} source={} path={path}", name, row.source)];
                    if let Some(key) = key {
                        lines.push(format!("uninstall-key={key}"));
                    }
                    lines
                })
                .collect();
            Some(
                Finding::new(
                    ids::TOOL_GHOST,
                    Severity::Warn,
                    format!(
                        "{} 有 {} 条注册表记录说它装过，而记录里的路径已经不在 —— 幽灵条目只报告，绝不替你删",
                        name,
                        rows.len()
                    ),
                    evidence,
                    sources::REGISTRY,
                )
                .with_confidence(Confidence::RegisteredMissing.as_str()),
            )
        })
        .collect()
}

/// `PrefixOrigin` 的 ASCII slug（`evidence` 里不许有中文）。
///
/// 从环境变量知道的那两档带上变量名："它是怎么知道的"决定了这条结论有多硬 ——
/// `probe` 是工具**自己说的**，`manager-link-var:NVM_SYMLINK` 是我们**推断**的。
fn origin_slug(origin: &PrefixOrigin) -> String {
    match origin {
        PrefixOrigin::Probe => "probe".to_owned(),
        PrefixOrigin::EnvVar(name) => format!("env-var:{name}"),
        PrefixOrigin::ManagerLinkVar(name) => format!("manager-link-var:{name}"),
    }
}

/// `tool.global-prefix-inside-version-dir`（**error**）：全局包前缀落在版本目录 /
/// reparse point 里。
///
/// # 严重度为什么是 error
///
/// **静默失效**：本机 `npm config get prefix` 是 `C:\nvm4w\nodejs` —— nvm4w 的
/// "当前版本"符号链接，指向 `…\nvm\v24.19.0`。用户切版本之前装的全局包全在
/// 那个版本目录里；切一次版本，它们就跟着旧版本一起从 `PATH` 上消失，而
/// `npm ls -g` 会说"一个包都没有"。用户不会察觉，因为他什么都没删。
///
/// # 两个触发条件，任一成立就报
///
/// `inside_reparse`（前缀本身是个链接）或 `version_component`（前缀或它的目标里有
/// `v24.19.0` 这种成分）—— 后者是同一件事在**没有符号链接**时的形态（直接把前缀
/// 设成某个版本目录）。
///
/// # 反例为什么健康
///
/// `C:\Program Files\nodejs` 这种普通目录：既不是链接，路径里也没有版本号成分，
/// 切版本动不到它。
///
/// # 来源为什么是 `manager`
///
/// 这条结论说的是**第三方管理器的全局包前缀**在哪；它是怎么知道的写在
/// `evidence` 的 `origin=` 那一行里（`probe` / `env-var:…` / `manager-link-var:…`）。
fn prefixes_inside_version_dirs(prefixes: &[GlobalPrefix]) -> Vec<Finding> {
    prefixes
        .iter()
        .filter(|prefix| prefix.inside_reparse || prefix.version_component.is_some())
        .map(|prefix| {
            let mut evidence = vec![
                format!("tool={} prefix={}", prefix.tool_id, prefix.prefix),
                format!("origin={}", origin_slug(&prefix.origin)),
            ];
            if let Some(kind) = prefix.reparse_kind {
                evidence.push(format!("reparse={kind}"));
            }
            if let Some(target) = prefix.link_target.as_deref() {
                evidence.push(format!("link-target={target}"));
            }
            if let Some(version) = prefix.version_component.as_deref() {
                evidence.push(format!("version={version}"));
            }
            Finding::new(
                ids::TOOL_GLOBAL_PREFIX_INSIDE_VERSION_DIR,
                Severity::Error,
                format!(
                    "{} 的全局包前缀 {} 落在版本目录 / reparse point 里 —— 切版本会把这批全局安装的包静默藏起来：它们会跟着旧版本一起离开 PATH，而工具自己会说「一个包都没有」",
                    prefix.tool_id, prefix.prefix
                ),
                evidence,
                sources::MANAGER,
            )
        })
        .collect()
}

/// `tool.unmanaged-directory`（info）：扫描根下面那些"看起来是开发工具、却没有任何
/// 机制在管"的目录。
///
/// # 判据是 `managed_by` 为空，而不是"没人提到过"
///
/// `mentioned_by` 与 `managed_by` 是两件事（见 [`DevRoot`] 的文档）：**只有
/// `manager` 与 `tuoen` 算"管"** —— 前者能把它装回来，后者是我们自己装的。
/// `path-resolution` / `app-paths` / `registry-arp` 只说明"我们知道它"，
/// `filesystem-scan` 更是只看了一眼。
///
/// 真机上 `C:\Dev\base\JDK` 就是"被提到但没人管"：`PATH` 上确实有
/// `C:\Dev\base\JDK\JDK8\bin\java.exe`（所以它被提到），但没有任何管理器或注册表
/// 负责把它装回来 —— **它仍然是"没人管"**。把"提过"当成"管着"会让这条检查在真机上
/// 漏掉一半该报的东西。
///
/// # 三档，三句话
///
/// 1. `only_scan_mentions_it`：只有文件系统扫描知道它。本机
///    `C:\Dev\Tool\apache-maven-3.9.5`（1.4 GB，而 `~/.m2/repository` 461 MB 证明它在用）。
/// 2. 被别的机制提到过、但没人管（本机 `C:\Dev\base\JDK`）。
/// 3. 谁都没提过。
///
/// 三档都报，`message` 分开写 —— 合并成一句话会让第一种听起来比实际更糟，
/// 而第二、三种听起来比实际更好。
///
/// # 反例为什么健康
///
/// `managed_by` 里有 `manager` 或 `tuoen` 的目录**是有人管的**：换一台机器能装回来
/// （这正是这条检查要说的那件事 —— 换机器重建不出来）。一条永远报错的检查会训练
/// 用户忽略所有发现，所以这一档必须排除。
///
/// # 严重度为什么只是 info
///
/// 它不影响任何命令能不能跑，是"换机器时最可能丢掉的东西"那一类。噪声级别。
fn unmanaged_directories(roots: &[DevRoot]) -> Vec<Finding> {
    roots
        .iter()
        .filter(|root| root.managed_by.is_empty())
        .map(|root| {
            // **分母必须说出来**：谁提过它、谁在管它。两条一起看，
            // "为什么你说没人管"这个问题就不需要再去读代码。
            let listed = |mechanisms: &[&'static str]| {
                if mechanisms.is_empty() {
                    "none".to_owned()
                } else {
                    mechanisms.join(",")
                }
            };
            let evidence = vec![
                root.path.clone(),
                format!("looks-like={}", root.looks_like),
                format!("children={}", root.children),
                format!("mentioned-by={}", listed(&root.mentioned_by)),
                format!("managed-by={}", listed(&root.managed_by)),
            ];
            let message = if root.only_scan_mentions_it {
                format!(
                    "{} 只有文件系统扫描知道它 —— 注册表、PATH、App Paths、任何管理器都没提过它，换一台机器重建不出来",
                    root.path
                )
            } else if root.mentioned_by.is_empty() {
                format!(
                    "{} 谁都没提过它 —— 只是名字或内容看起来像开发工具，换一台机器更重建不出来",
                    root.path
                )
            } else {
                format!(
                    "{} 被别的机制看到过（{}），但没有任何机制负责把它装回来 —— 换一台机器重建不出来",
                    root.path,
                    root.mentioned_by.join(" / ")
                )
            };
            Finding::new(
                ids::TOOL_UNMANAGED_DIRECTORY,
                Severity::Info,
                message,
                evidence,
                sources::FILESYSTEM,
            )
            .with_confidence(Confidence::DirectoryOnly.as_str())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tuoen_platform::fixture::{
        FakeMachine, FixtureDir, FixtureKey, FixturePath, MachineFixture,
    };
    use tuoen_platform::{FileSystem as _, RegHive, RegValue, Registry as _, ReparseKind};

    use crate::capture::test_support::CaptureFixture;
    use crate::capture::{EnvFile, PathFile, SCHEMA_VERSION, ToolRow, ToolsFile, WslFile};
    use crate::detect::Confidence;
    use crate::doctor::facts::{CommandResolution, DevRoot, GlobalPrefix, PrefixOrigin};
    use crate::doctor::{MachineFacts, Severity, ShimFacts, SystemFacts, ids, sources};

    use super::{WEAKEST_TO_STRONGEST, run};

    /// 固定的捕获时间 —— 用例里时间戳是**参数**，所以可以钉死。
    const AT: &str = "2026-10-02T12:00:00Z";

    /// 一条工具记录，只写用例关心的字段。
    fn tool_row(
        name: &str,
        path: &str,
        source: &str,
        confidence: &str,
        manager: Option<&str>,
    ) -> ToolRow {
        ToolRow {
            name: name.to_owned(),
            version: None,
            path: path.to_owned(),
            source: source.to_owned(),
            confidence: confidence.to_owned(),
            manager: manager.map(str::to_owned),
            evidence: "用例造的一条记录".to_owned(),
            reproducible: matches!(confidence, "managed" | "executable" | "registered"),
        }
    }

    /// 一条命令解析结果。
    fn resolution(tool: &str, command: &str, dir: &str) -> CommandResolution {
        CommandResolution {
            tool_id: tool.to_owned(),
            command: command.to_owned(),
            file: format!("{command}.exe"),
            directory: Some(dir.to_owned()),
            scope: Some("machine"),
            index: Some(0),
            primary: command == tool,
        }
    }

    /// 另外三个文件形状 —— 从**真的采集器**里拿一份空机器的产物，而不是手写结构体
    /// （手写的那一份会在别人加字段时静默漂移）。
    fn other_files() -> (PathFile, WslFile) {
        let bundle = CaptureFixture::build(&MachineFixture::default()).capture_all(AT);
        (
            bundle.path.expect("path.toml"),
            bundle.wsl.expect("wsl.toml"),
        )
    }

    /// 一份「事实」，四个活事实列留空 —— 每条检查只读它自己那一列。
    fn facts_with(tools: ToolsFile) -> MachineFacts {
        let (path, wsl) = other_files();
        MachineFacts {
            path,
            env: EnvFile::new(AT),
            tools,
            wsl,
            system: SystemFacts::default(),
            resolution: Vec::new(),
            global_prefix: Vec::new(),
            dev_roots: Vec::new(),
            shims: ShimFacts::default(),
        }
    }

    fn tools_file(rows: Vec<ToolRow>) -> ToolsFile {
        ToolsFile {
            schema_version: SCHEMA_VERSION,
            captured_at: AT.to_owned(),
            tool: rows,
        }
    }

    // ── tool.multi-manager ──────────────────────────────────────────────────

    /// 正例：node 同时被 nvm4w（`manager`）管、被我们（`tuoen`）管、还能在 `PATH`
    /// 上解析到 —— 本机就是这个形状。
    ///
    /// **`path-resolution` 那一行不进 `evidence`**：它不是管理器。这正是收窄后的
    /// 判据（决策 102）—— 一个工具既在 `PATH` 上又被某个管理器管着是**正常**的。
    #[test]
    fn a_tool_held_by_two_managers_is_an_error() {
        let facts = facts_with(tools_file(vec![
            tool_row(
                "node",
                r"C:\Users\x\AppData\Local\nvm",
                "manager",
                "manager-owned",
                Some("nvm4w"),
            ),
            tool_row(
                "node",
                r"C:\Users\x\AppData\Local\tuoen\store\node\versions\24.19.0",
                "tuoen",
                "managed",
                None,
            ),
            tool_row(
                "node",
                r"C:\nvm4w\nodejs\node.exe",
                "path-resolution",
                "executable",
                None,
            ),
        ]));

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_MULTI_MANAGER);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(
            findings[0].confidence,
            Some("manager-owned"),
            "取这批行里最弱的一档"
        );
        assert_eq!(
            findings[0].evidence,
            vec![
                "node manager=nvm4w".to_owned(),
                "node manager=tuoen".to_owned(),
            ],
            "只列**管理器**；`path-resolution` 那一行不进证据"
        );
    }

    /// **反例（收窄后的判据，决策 102）**：三个来源、**零个管理器** —— 本机的 `java`
    /// 就是这个形状（`PATH` 解析 + 卸载键 + 手工解压目录的扫描），而它是**健康的**：
    /// "既在 `PATH` 上又在卸载键里"不是"两个人在管"。
    #[test]
    fn three_mechanisms_without_a_manager_are_not_multi_manager() {
        let facts = facts_with(tools_file(vec![
            tool_row(
                "java",
                r"C:\Dev\base\JDK\JDK8\bin\java.exe",
                "path-resolution",
                "executable",
                None,
            ),
            tool_row(
                "java",
                r"C:\Program Files\Java\jre1.8.0_491\",
                "registry-arp",
                "registered",
                None,
            ),
            tool_row(
                "java",
                r"C:\Dev\base\JDK",
                "filesystem-scan",
                "directory-only",
                None,
            ),
        ]));

        let findings = run(&facts);
        assert!(
            findings
                .iter()
                .all(|finding| finding.id != ids::TOOL_MULTI_MANAGER),
            "三个来源、零个管理器不是多管理器：{findings:?}"
        );
    }

    /// 一个管理器 + 一堆别的来源仍然**不算**多管理器（哪怕它有五行）。
    #[test]
    fn one_manager_with_many_other_sources_is_healthy() {
        let facts = facts_with(tools_file(vec![
            tool_row(
                "python",
                "<uv 管理，版本库位置未知>",
                "manager",
                "manager-owned",
                Some("uv"),
            ),
            tool_row(
                "python",
                r"C:\Users\x\AppData\Local\Programs\Python\Python312\python.exe",
                "path-resolution",
                "executable",
                None,
            ),
            tool_row(
                "python",
                r"C:\Users\x\AppData\Local\Programs\Python\Python311\",
                "registry-arp",
                "registered-missing",
                None,
            ),
        ]));

        let findings = run(&facts);
        assert!(
            findings
                .iter()
                .all(|finding| finding.id != ids::TOOL_MULTI_MANAGER),
            "只有一个管理器就不是多管理器：{findings:?}"
        );
    }

    /// 反例：同一个机制报了多行 —— 那只是"同一个机制看见了多个安装"。
    #[test]
    fn many_rows_from_one_mechanism_are_not_multi_manager() {
        let facts = facts_with(tools_file(vec![
            tool_row(
                "node",
                r"C:\a\node.exe",
                "path-resolution",
                "executable",
                None,
            ),
            tool_row(
                "node",
                r"C:\b\node.exe",
                "path-resolution",
                "executable",
                None,
            ),
            tool_row(
                "node",
                r"C:\c\node.exe",
                "path-resolution",
                "executable",
                None,
            ),
        ]));

        let findings = run(&facts);
        assert!(
            findings
                .iter()
                .all(|finding| finding.id != ids::TOOL_MULTI_MANAGER),
            "行数多不等于机制多：{findings:?}"
        );
    }

    // ── tool.multiple-active ────────────────────────────────────────────────

    /// 正例：本机 `java` 的分裂 —— `java` 在 Oracle 的 `java8path`，`javac` / `jar`
    /// 在手工解压的 JDK8。
    #[test]
    fn java_split_across_two_directories_is_an_error() {
        let mut facts = facts_with(tools_file(vec![tool_row(
            "java",
            r"C:\Dev\base\JDK\JDK8\bin\java.exe",
            "path-resolution",
            "executable",
            None,
        )]));
        facts.resolution = vec![
            resolution(
                "java",
                "java",
                r"C:\Program Files (x86)\Common Files\Oracle\Java\java8path",
            ),
            resolution("java", "javac", r"C:\Dev\base\JDK\JDK8\bin"),
            resolution("java", "jar", r"C:\Dev\base\JDK\JDK8\bin"),
        ];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_MULTIPLE_ACTIVE);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].source, sources::PATH_RESOLUTION);
        assert_eq!(findings[0].confidence, Some("executable"));
        assert_eq!(
            findings[0].evidence,
            vec![
                r"tool=java command=java dir=C:\Program Files (x86)\Common Files\Oracle\Java\java8path"
                    .to_owned(),
                r"tool=java command=javac dir=C:\Dev\base\JDK\JDK8\bin".to_owned(),
            ],
            "一个目录一行：`jar` 与 `javac` 同一个目录，不重复报"
        );
    }

    /// 反例：同一个工具的所有命令都在同一个目录里。
    #[test]
    fn one_directory_for_every_command_is_healthy() {
        let mut facts = facts_with(tools_file(vec![tool_row(
            "node",
            r"C:\nvm4w\nodejs\node.exe",
            "path-resolution",
            "executable",
            None,
        )]));
        facts.resolution = vec![
            resolution("node", "node", r"C:\nvm4w\nodejs"),
            resolution("node", "npm", r"C:\nvm4w\nodejs"),
            resolution("node", "npx", r"C:\nvm4w\nodejs"),
        ];

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    /// 反例：不在 `PATH` 上的命令不是"另一套安装"的证据。
    #[test]
    fn commands_that_are_not_on_path_are_not_a_second_install() {
        let mut facts = facts_with(ToolsFile::new(AT));
        let mut missing = resolution("java", "javac", r"C:\Dev\base\JDK\JDK8\bin");
        missing.directory = None;
        missing.scope = None;
        missing.index = None;
        facts.resolution = vec![
            resolution(
                "java",
                "java",
                r"C:\Program Files (x86)\Common Files\Oracle\Java\java8path",
            ),
            missing,
        ];

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    /// 反例：同一个目录的两种拼法不是两套安装（Windows 的路径比较忽略大小写）。
    #[test]
    fn two_spellings_of_the_same_directory_are_one_install() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.resolution = vec![
            resolution("node", "node", r"C:\nvm4w\nodejs"),
            resolution("node", "npm", r"c:\NVM4W\nodejs\"),
        ];

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    /// **本机 `python` 的形状必须报，而且不许被"优化"掉。**
    ///
    /// `python` 在 `WindowsApps`（0 字节别名），`pip` 在 `Python312\Scripts`。
    /// 有人会觉得"那只是个别名，过滤掉吧" —— 过滤掉正好把这条检查最真实的
    /// 一次命中删掉：`pip` 装的东西与 `python` 跑的东西确实不是同一套。
    #[test]
    fn the_windowsapps_alias_directory_still_counts_as_a_second_install() {
        let mut facts = facts_with(tools_file(vec![tool_row(
            "python",
            r"C:\Users\x\AppData\Local\Programs\Python\Python312\python.exe",
            "path-resolution",
            "executable",
            None,
        )]));
        facts.resolution = vec![
            resolution(
                "python",
                "python",
                r"C:\Users\x\AppData\Local\Microsoft\WindowsApps",
            ),
            resolution(
                "python",
                "python3",
                r"C:\Users\x\AppData\Local\Microsoft\WindowsApps",
            ),
            resolution(
                "python",
                "pip",
                r"C:\Users\x\AppData\Local\Programs\Python\Python312\Scripts",
            ),
            resolution(
                "python",
                "pip3",
                r"C:\Users\x\AppData\Local\Programs\Python\Python312\Scripts",
            ),
        ];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_MULTIPLE_ACTIVE);
        assert_eq!(findings[0].evidence.len(), 2, "两个目录，两条");
    }

    // ── tool.ghost ──────────────────────────────────────────────────────────

    /// 把假机器**可观察状态**读一遍，作为"跑之前 / 跑之后"的比对基准。
    ///
    /// 顶层子键那一行是必要的：只列"我们声明过的键"看不见"凭空写了一个新键"。
    /// 这是 `AGENTS.md` 那条"允许列表要往被测代码会写的那个地方看一层"的同一条教训。
    fn snapshot(machine: &FakeMachine, description: &MachineFixture) -> Vec<String> {
        let mut out = Vec::new();
        for hive in [RegHive::Hkcu, RegHive::Hklm] {
            out.push(format!(
                "top-level {hive:?} = {:?}",
                machine.registry.subkeys(hive, "").unwrap_or_default()
            ));
        }
        for key in &description.registry {
            out.push(format!(
                "key {:?} {} = {:?}",
                key.hive,
                key.path,
                machine
                    .registry
                    .values(key.hive, &key.path)
                    .unwrap_or_default()
            ));
        }
        for dir in &description.dirs {
            out.push(format!(
                "dir {} = {:?}",
                dir.path,
                machine.fs.list_dir(Path::new(&dir.path))
            ));
        }
        for path in &description.paths {
            out.push(format!(
                "path {} = {:?}",
                path.path,
                machine.fs.inspect(Path::new(&path.path))
            ));
        }
        out
    }

    /// 本机 `Python 3.11.9` 的形状：卸载键在、`InstallLocation` 在、**那个目录不在**。
    fn ghost_machine() -> MachineFixture {
        MachineFixture {
            registry: vec![FixtureKey::new(
                RegHive::Hklm,
                r"Microsoft\Windows\CurrentVersion\Uninstall\{GHOST-PYTHON311}",
                [
                    (
                        "DisplayName".to_owned(),
                        RegValue::Sz("Python 3.11.9 (64-bit)".to_owned()),
                    ),
                    (
                        "InstallLocation".to_owned(),
                        RegValue::Sz(
                            r"C:\Users\x\AppData\Local\Programs\Python\Python311\".to_owned(),
                        ),
                    ),
                    (
                        "DisplayVersion".to_owned(),
                        RegValue::Sz("3.11.9150.0".to_owned()),
                    ),
                ]
                .into(),
            )],
            ..MachineFixture::default()
        }
    }

    /// 正例（走**真的采集器**）：注册表说装了、目录不在 → 报幽灵，而且
    /// **跑完前后注册表与文件系统逐项没变**。
    #[test]
    fn a_registry_entry_whose_files_are_gone_is_a_ghost() {
        let description = ghost_machine();
        let fixture = CaptureFixture::build(&description);
        let before = snapshot(&fixture.detect.machine, &description);

        let bundle = fixture.capture_all(AT);
        let facts = MachineFacts {
            tools: bundle.tools.expect("tools.toml"),
            ..facts_with(ToolsFile::new(AT))
        };
        let findings = run(&facts);

        let ghosts: Vec<_> = findings
            .iter()
            .filter(|finding| finding.id == ids::TOOL_GHOST)
            .collect();
        assert_eq!(ghosts.len(), 1, "{findings:?}");
        assert_eq!(ghosts[0].severity, Severity::Warn);
        assert_eq!(ghosts[0].confidence, Some("registered-missing"));
        assert_eq!(ghosts[0].source, sources::REGISTRY);
        assert_eq!(
            ghosts[0].evidence,
            vec![
                r"tool=python source=registry-arp path=C:\Users\x\AppData\Local\Programs\Python\Python311\"
                    .to_owned()
            ],
            "幽灵的路径必须原样出现在 evidence 里"
        );
        assert!(
            ghosts[0].message.contains("绝不替你删"),
            "message 要说清这条检查不动手：{}",
            ghosts[0].message
        );

        // **声称没碰 ≠ 证明没碰**：前后各读一遍，逐项相同。
        assert_eq!(
            before,
            snapshot(&fixture.detect.machine, &description),
            "检查项不许写注册表、不许写文件系统"
        );
    }

    /// 反例（同样走真采集器）：目录真的在 → `registered`，不是幽灵。
    #[test]
    fn a_registry_entry_whose_directory_is_there_is_not_a_ghost() {
        let mut description = ghost_machine();
        description.dirs.push(FixtureDir::new(
            r"C:\Users\x\AppData\Local\Programs\Python\Python311",
            vec![FixturePath::file("python.exe", 102_400)],
        ));
        let fixture = CaptureFixture::build(&description);
        let bundle = fixture.capture_all(AT);
        let facts = MachineFacts {
            tools: bundle.tools.expect("tools.toml"),
            ..facts_with(ToolsFile::new(AT))
        };

        assert!(
            run(&facts)
                .iter()
                .all(|finding| finding.id != ids::TOOL_GHOST),
            "注册表的说法与磁盘一致时不该报幽灵"
        );
        assert!(
            facts
                .tools
                .tool
                .iter()
                .any(|row| row.confidence == "registered"),
            "对照组的置信度必须是 registered：{:?}",
            facts.tools.tool
        );
    }

    /// 反例（手写）：`registered` / `executable` 都不是幽灵。
    #[test]
    fn a_healthy_row_is_never_a_ghost() {
        let facts = facts_with(tools_file(vec![
            tool_row(
                "java",
                r"C:\Program Files\Java\jre1.8.0_491\",
                "registry-arp",
                "registered",
                None,
            ),
            tool_row(
                "node",
                r"C:\nvm4w\nodejs\node.exe",
                "path-resolution",
                "executable",
                None,
            ),
        ]));

        assert!(
            run(&facts)
                .iter()
                .all(|finding| finding.id != ids::TOOL_GHOST),
            "{:?}",
            run(&facts)
        );
    }

    /// 一个工具的多条幽灵路径合并成**一条** finding（本机 `python` 2 行、
    /// `dotnet` 3 行）。
    #[test]
    fn ghost_rows_are_grouped_into_one_finding_per_tool() {
        let facts = facts_with(tools_file(vec![
            tool_row(
                "python",
                r"C:\Users\x\AppData\Local\Programs\Python\Python311\",
                "registry-arp",
                "registered-missing",
                None,
            ),
            tool_row(
                "python",
                r"C:\Users\x\AppData\Local\Programs\Python\Python311\python.exe",
                "path-resolution",
                "registered-missing",
                None,
            ),
            tool_row(
                "dotnet",
                r"C:\Program Files\dotnet\",
                "registry-arp",
                "registered-missing",
                None,
            ),
            tool_row(
                "dotnet",
                r"C:\Program Files\dotnet\x64",
                "registry-arp",
                "registered-missing",
                None,
            ),
            tool_row(
                "dotnet",
                r"C:\Program Files\dotnet\sdk",
                "app-paths",
                "registered-missing",
                None,
            ),
        ]));

        // 这几行来自两个机制（`registry-arp` + `path-resolution` / `app-paths`），
        // 所以它们**同时**会命中 `tool.multi-manager` —— 那是真的（本机 `python`
        // 就是"2 条 PATH + 9 个卸载键"）。这条用例只钉"一个工具一条幽灵"。
        let findings: Vec<_> = run(&facts)
            .into_iter()
            .filter(|finding| finding.id == ids::TOOL_GHOST)
            .collect();
        assert_eq!(findings.len(), 2, "一个工具一条：{findings:?}");
        for finding in &findings {
            assert_eq!(finding.confidence, Some("registered-missing"));
        }
        let python = findings
            .iter()
            .find(|finding| finding.evidence[0].starts_with("tool=python"))
            .expect("python 的两行");
        assert_eq!(python.evidence.len(), 2);
        let dotnet = findings
            .iter()
            .find(|finding| finding.evidence[0].starts_with("tool=dotnet"))
            .expect("dotnet 的三行");
        assert_eq!(dotnet.evidence.len(), 3);
    }

    /// **本机真实形状**：没有 `InstallLocation` 的幽灵，`path` 那一列是引擎的
    /// **中文占位符**（`<无 InstallLocation，卸载键 {GUID}>`）。
    ///
    /// `--json` 里不许有 CJK，所以占位符不能原样进 `evidence`；但那个卸载键是
    /// 唯一能定位这条记录的东西，必须留下 —— 于是 `<placeholder>` + `uninstall-key=`。
    /// 这条用例同时钉住两件事：**没有 CJK**、**键没丢**。
    #[test]
    fn a_ghost_without_a_location_keeps_its_uninstall_key_as_ascii_evidence() {
        let placeholder = "<无 InstallLocation，卸载键 {665A0435-D5D5-4A49-9DE0-FBC23C5425ED}>";
        let facts = facts_with(tools_file(vec![tool_row(
            "python",
            placeholder,
            "registry-arp",
            "registered-missing",
            None,
        )]));

        let findings: Vec<_> = run(&facts)
            .into_iter()
            .filter(|finding| finding.id == ids::TOOL_GHOST)
            .collect();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(
            findings[0].evidence,
            vec![
                "tool=python source=registry-arp path=<placeholder>".to_owned(),
                "uninstall-key={665A0435-D5D5-4A49-9DE0-FBC23C5425ED}".to_owned(),
            ]
        );
        assert!(
            findings[0].evidence.iter().all(|line| line.is_ascii()),
            "占位符里的中文不许进 evidence：{:?}",
            findings[0].evidence
        );
    }

    /// 反例：一条**真的**带中文的路径（中文用户名）也不许把 CJK 带进 `evidence`，
    /// 但它不是一个占位符，所以标签要说清这一点。
    #[test]
    fn a_real_non_ascii_path_is_labelled_not_printed() {
        let facts = facts_with(tools_file(vec![tool_row(
            "node",
            r"C:\Users\张三\tools\node\node.exe",
            "path-resolution",
            "registered-missing",
            None,
        )]));

        let findings: Vec<_> = run(&facts)
            .into_iter()
            .filter(|finding| finding.id == ids::TOOL_GHOST)
            .collect();
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(
            findings[0].evidence,
            vec!["tool=node source=path-resolution path=<non-ascii>".to_owned()],
            "真的非 ASCII 路径与引擎的占位符必须能被区分开"
        );
    }

    // ── tool.global-prefix-inside-version-dir ───────────────────────────────

    /// 一个全局包前缀。
    fn prefix(
        tool_id: &str,
        prefix: &str,
        origin: PrefixOrigin,
        inside_reparse: bool,
        link_target: Option<&str>,
        version_component: Option<&str>,
    ) -> GlobalPrefix {
        GlobalPrefix {
            tool_id: tool_id.to_owned(),
            prefix: prefix.to_owned(),
            origin,
            inside_reparse,
            reparse_kind: inside_reparse.then_some(ReparseKind::SymlinkDir),
            link_target: link_target.map(str::to_owned),
            version_component: version_component.map(str::to_owned),
        }
    }

    /// 正例：本机 `npm` 的全局前缀 —— 一个指向版本目录的符号链接。
    #[test]
    fn a_global_prefix_inside_a_version_symlink_is_an_error() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.global_prefix = vec![prefix(
            "node",
            r"C:\nvm4w\nodejs",
            PrefixOrigin::Probe,
            true,
            Some(r"C:\Users\x\AppData\Local\nvm\v24.19.0"),
            Some("24.19.0"),
        )];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_GLOBAL_PREFIX_INSIDE_VERSION_DIR);
        assert_eq!(findings[0].severity, Severity::Error);
        assert_eq!(findings[0].source, sources::MANAGER);
        assert_eq!(
            findings[0].evidence,
            vec![
                r"tool=node prefix=C:\nvm4w\nodejs".to_owned(),
                "origin=probe".to_owned(),
                "reparse=symlink-dir".to_owned(),
                r"link-target=C:\Users\x\AppData\Local\nvm\v24.19.0".to_owned(),
                "version=24.19.0".to_owned(),
            ]
        );
        assert!(
            findings[0].message.contains("静默"),
            "message 要说清「静默」这件事：{}",
            findings[0].message
        );
    }

    /// 正例②：没有符号链接、但前缀的路径里就有版本号 —— 同一个问题的另一种形态。
    #[test]
    fn a_version_component_alone_is_enough() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.global_prefix = vec![prefix(
            "node",
            r"C:\Program Files\nodejs\v20.11.0",
            PrefixOrigin::EnvVar("NPM_CONFIG_PREFIX".to_owned()),
            false,
            None,
            Some("20.11.0"),
        )];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_GLOBAL_PREFIX_INSIDE_VERSION_DIR);
        assert_eq!(
            findings[0].evidence,
            vec![
                r"tool=node prefix=C:\Program Files\nodejs\v20.11.0".to_owned(),
                "origin=env-var:NPM_CONFIG_PREFIX".to_owned(),
                "version=20.11.0".to_owned(),
            ],
            "没有 reparse 时不该有 reparse= 那一行"
        );
    }

    /// 反例：普通目录、没有版本成分。
    #[test]
    fn a_plain_prefix_directory_is_healthy() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.global_prefix = vec![prefix(
            "node",
            r"C:\Program Files\nodejs",
            PrefixOrigin::EnvVar("NPM_CONFIG_PREFIX".to_owned()),
            false,
            None,
            None,
        )];

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    /// `origin` 这一行是给脚本读的数据：三档各有稳定的 ASCII slug。
    #[test]
    fn every_origin_has_a_stable_ascii_slug() {
        let origins = [
            (PrefixOrigin::Probe, "origin=probe"),
            (
                PrefixOrigin::EnvVar("NPM_CONFIG_PREFIX".to_owned()),
                "origin=env-var:NPM_CONFIG_PREFIX",
            ),
            (
                PrefixOrigin::ManagerLinkVar("NVM_SYMLINK".to_owned()),
                "origin=manager-link-var:NVM_SYMLINK",
            ),
        ];
        for (origin, expected) in origins {
            let mut facts = facts_with(ToolsFile::new(AT));
            facts.global_prefix =
                vec![prefix("node", r"C:\nvm4w\nodejs", origin, true, None, None)];

            let findings = run(&facts);
            assert_eq!(findings.len(), 1, "{findings:?}");
            assert!(
                findings[0].evidence.iter().any(|line| line == expected),
                "期望 {expected}，实际 {:?}",
                findings[0].evidence
            );
        }
    }

    // ── tool.unmanaged-directory ────────────────────────────────────────────

    /// 一个"没人管"的目录。
    fn dev_root(
        path: &str,
        looks_like: &'static str,
        children: usize,
        mentioned_by: Vec<&'static str>,
        managed_by: Vec<&'static str>,
        only_scan_mentions_it: bool,
    ) -> DevRoot {
        DevRoot {
            path: path.to_owned(),
            looks_like,
            children,
            mentioned_by,
            managed_by,
            only_scan_mentions_it,
        }
    }

    /// 正例①：只有文件系统扫描知道它（本机 Maven 的形状）。
    #[test]
    fn a_directory_only_the_scanner_knows_is_reported() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.dev_roots = vec![dev_root(
            r"C:\Dev\Tool\apache-maven-3.9.5",
            "scan-only",
            8,
            vec!["filesystem-scan"],
            Vec::new(),
            true,
        )];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_UNMANAGED_DIRECTORY);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].confidence, Some("directory-only"));
        assert_eq!(
            findings[0].evidence,
            vec![
                r"C:\Dev\Tool\apache-maven-3.9.5".to_owned(),
                "looks-like=scan-only".to_owned(),
                "children=8".to_owned(),
                "mentioned-by=filesystem-scan".to_owned(),
                "managed-by=none".to_owned(),
            ]
        );
        assert!(
            findings[0].message.contains("重建不出来"),
            "message 要说清「换一台机器重建不出来」：{}",
            findings[0].message
        );
    }

    /// 正例②：**被提到 ≠ 有人管**（本机 `C:\Dev\base\JDK` 的形状）。
    ///
    /// `PATH` 上确实有 `…\JDK8\bin\java.exe`，所以它被 `path-resolution` 提到了；
    /// 但没有任何管理器或注册表负责把它装回来 —— 它仍然是"没人管"，仍然要报，
    /// 只是说法不一样。
    #[test]
    fn a_directory_seen_on_path_but_managed_by_nobody_is_still_reported() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.dev_roots = vec![dev_root(
            r"C:\Dev\base\JDK",
            "scan-only",
            3,
            vec!["filesystem-scan", "path-resolution"],
            Vec::new(),
            false,
        )];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_UNMANAGED_DIRECTORY);
        assert_eq!(
            findings[0].evidence,
            vec![
                r"C:\Dev\base\JDK".to_owned(),
                "looks-like=scan-only".to_owned(),
                "children=3".to_owned(),
                "mentioned-by=filesystem-scan,path-resolution".to_owned(),
                "managed-by=none".to_owned(),
            ]
        );
        assert!(
            findings[0].message.contains("没有任何机制负责把它装回来"),
            "「被提到」与「有人管」必须分开说：{}",
            findings[0].message
        );

        // 与"只有扫描知道它"那一档不是同一句话。
        let mut only_scan = facts_with(ToolsFile::new(AT));
        only_scan.dev_roots = vec![dev_root(
            r"C:\Dev\base\JDK",
            "scan-only",
            3,
            vec!["filesystem-scan"],
            Vec::new(),
            true,
        )];
        assert_ne!(
            run(&only_scan)[0].message,
            findings[0].message,
            "两档不能合并成一句话"
        );
    }

    /// 正例③：谁都没提过的那一档 —— 也报，但说法必须与上面两档不同。
    #[test]
    fn a_directory_nobody_mentioned_is_reported_with_a_different_sentence() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.dev_roots = vec![dev_root(
            r"C:\Dev\Tool\mystery",
            "name-matches-tool",
            3,
            Vec::new(),
            Vec::new(),
            false,
        )];

        let findings = run(&facts);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].id, ids::TOOL_UNMANAGED_DIRECTORY);
        assert_eq!(findings[0].confidence, Some("directory-only"));
        assert_eq!(
            findings[0].evidence,
            vec![
                r"C:\Dev\Tool\mystery".to_owned(),
                "looks-like=name-matches-tool".to_owned(),
                "children=3".to_owned(),
                "mentioned-by=none".to_owned(),
                "managed-by=none".to_owned(),
            ]
        );
        assert!(
            findings[0].message.contains("谁都没提过"),
            "三档的 message 必须分开：{}",
            findings[0].message
        );
    }

    /// 反例：有机制**在管**它（`manager` / `tuoen`）—— 换一台机器能装回来，
    /// 这条检查要说的那件事不成立。
    #[test]
    fn a_directory_a_manager_manages_is_not_unmanaged() {
        let mut facts = facts_with(ToolsFile::new(AT));
        facts.dev_roots = vec![
            dev_root(
                r"C:\nvm4w\nodejs",
                "scan-only",
                5,
                vec!["filesystem-scan", "path-resolution", "manager"],
                vec!["manager"],
                false,
            ),
            dev_root(
                r"C:\Users\x\AppData\Local\tuoen\store\node\versions\24.19.0",
                "scan-only",
                4,
                vec!["tuoen"],
                vec!["tuoen"],
                false,
            ),
        ];

        assert!(run(&facts).is_empty(), "{:?}", run(&facts));
    }

    // ── 跨检查的性质 ────────────────────────────────────────────────────────

    /// 弱弱次序表必须与检测引擎的七档一一对应 —— 引擎加一档而这里忘了加，
    /// `weakest_confidence` 会静默把那一档当"不存在"。
    #[test]
    fn the_weakness_table_covers_every_confidence_level_exactly_once() {
        let levels = [
            Confidence::Managed,
            Confidence::Executable,
            Confidence::RegisteredMissing,
            Confidence::DirectoryOnly,
            Confidence::AliasGhost,
            Confidence::ManagerOwned,
            Confidence::Registered,
        ];
        for level in levels {
            assert_eq!(
                WEAKEST_TO_STRONGEST
                    .iter()
                    .filter(|slug| **slug == level.as_str())
                    .count(),
                1,
                "{} 在次序表里必须恰好出现一次",
                level.as_str()
            );
        }
        assert_eq!(WEAKEST_TO_STRONGEST.len(), levels.len());
    }

    /// **五条检查各响一次**的一台机器：`evidence` 全是 ASCII，`message` 是中文。
    #[test]
    fn evidence_is_ascii_data_while_the_message_is_chinese() {
        let mut facts = facts_with(tools_file(vec![
            tool_row(
                "node",
                r"C:\Users\x\AppData\Local\nvm",
                "manager",
                "manager-owned",
                Some("nvm4w"),
            ),
            tool_row(
                "node",
                r"C:\Users\x\AppData\Local\tuoen\store\node\versions\24.19.0",
                "tuoen",
                "managed",
                None,
            ),
            tool_row(
                "python",
                r"C:\Users\x\AppData\Local\Programs\Python\Python311\",
                "registry-arp",
                "registered-missing",
                None,
            ),
            // **本机真实形状**：没答出位置的那些幽灵，`path` 是中文占位符。
            // 它必须被 ASCII 化（否则整份 `--json` 就被弄脏了）。
            tool_row(
                "dotnet",
                "<无 InstallLocation，卸载键 {33CBD71A-9813-4440-92B8-B06C04852B34}>",
                "registry-arp",
                "registered-missing",
                None,
            ),
        ]));
        facts.resolution = vec![
            resolution(
                "java",
                "java",
                r"C:\Program Files (x86)\Common Files\Oracle\Java\java8path",
            ),
            resolution("java", "javac", r"C:\Dev\base\JDK\JDK8\bin"),
        ];
        facts.global_prefix = vec![prefix(
            "node",
            r"C:\nvm4w\nodejs",
            PrefixOrigin::Probe,
            true,
            Some(r"C:\Users\x\AppData\Local\nvm\v24.19.0"),
            Some("24.19.0"),
        )];
        facts.dev_roots = vec![dev_root(
            r"C:\Dev\Tool\apache-maven-3.9.5",
            "scan-only",
            8,
            vec!["filesystem-scan"],
            Vec::new(),
            true,
        )];

        let findings = run(&facts);
        let id_list: Vec<&str> = findings.iter().map(|finding| finding.id).collect();
        for expected in [
            ids::TOOL_MULTI_MANAGER,
            ids::TOOL_MULTIPLE_ACTIVE,
            ids::TOOL_GHOST,
            ids::TOOL_GLOBAL_PREFIX_INSIDE_VERSION_DIR,
            ids::TOOL_UNMANAGED_DIRECTORY,
        ] {
            assert!(id_list.contains(&expected), "少了 {expected}：{id_list:?}");
        }
        // 6 = 五条检查各响一次，其中 `tool.ghost` 是**一个工具一条**（python + dotnet）。
        assert_eq!(findings.len(), 6, "五条各响一次：{id_list:?}");

        for finding in &findings {
            for line in &finding.evidence {
                assert!(
                    line.is_ascii(),
                    "{} 的 evidence 里有非 ASCII：{line}",
                    finding.id
                );
            }
            assert!(
                finding.message.chars().any(|c| c as u32 > 0x2000),
                "{} 的 message 应当是中文：{}",
                finding.id,
                finding.message
            );
        }
    }

    /// `tool.ghost` 是票据里点名要"断言无写操作"的那一条，这里再钉一次它的
    /// 纯函数性质：同一份事实跑两次逐项相同。
    #[test]
    fn running_twice_gives_the_same_findings() {
        let description = ghost_machine();
        let bundle = CaptureFixture::build(&description).capture_all(AT);
        let facts = MachineFacts {
            tools: bundle.tools.expect("tools.toml"),
            ..facts_with(ToolsFile::new(AT))
        };

        assert_eq!(run(&facts), run(&facts));
    }
}
