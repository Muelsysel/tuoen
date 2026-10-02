//! 把声明**解析成确定版本**（决策 114）。
//!
//! # 候选从哪来
//!
//! | 来源 | 版本 | 要前置的目录 | `hash` |
//! |---|---|---|---|
//! | [`tuoen_store::installed_versions`] | 目录名 | 记录里 `layout.bin` 第一条的父目录（见下） | 记录里的 `sha256` |
//! | [`DetectedTool`] 里同名且版本已知的行 | 探测到的版本 | `DetectedTool::path`（是文件就取父目录） | 没有 |
//!
//! `layout.bin` 是"命令名 → 归档内相对路径"（`node` → `bin/node.exe`），
//! 于是**要前置的目录是那条相对路径的父目录**；`bin` 是空表就退到 `layout.home`，
//! 还没有就退到版本目录本身。三条退路的意义是：**前置目录可以是错的，但不能是空的** ——
//! 一个空串进 `PATH` 会让 Windows 把"当前目录"当成命令来源
//! （那是本仓库最不想制造的一种状态）。
//!
//! # 选哪一个（决策 114 的字面）
//!
//! 先按**规格过滤**（`spec.matches(version)`），再在剩下的里挑：
//!
//! 1. 版本号大的优先（[`tuoen_store::compare_versions`]）；
//! 2. **同一个版本号时 `tuoen` 优先**；
//! 3. 还平手（同版本、同是第三方）→ 来源名、路径，逐字比 —— 为了让结果**确定**
//!    （两行同名的 `PATH` 命中谁，不该取决于检测引擎那天的遍历顺序）。
//!
//! 第 1 条排在前面是决策 114 的原话（"同一版本号时 tuoen 优先；否则取最大的那个"）：
//! 一个更高的、满足声明的第三方版本会赢过我们自己装的低版本。
//!
//! # 失败是**两个**列表，不是一个
//!
//! [`Resolution::Failed`] 分开装"声明了但没装"（[`MissingTool`]）与"声明了但我不认识这个 id"
//! （决策 113 的 `UnknownTool`）。两者的修法完全不同（装一个 vs 改一行），
//! 合并成一个"解析失败"会让 CLI 只能给出含糊的提示。

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::detect::{DetectedTool, DetectionSource, ToolSpec};
use crate::pin::file::PinFile;
use crate::pin::trust::absolute_dir;

/// 解析一次所需的全部输入。
///
/// **三个字段都是借来的**：这个结构只描述"用哪些事实去解析"，不持有它们 ——
/// 于是"解析没读机器"这件事在签名上就看得出来（store 与检测结果都是调用方给的）。
#[derive(Debug)]
pub struct ResolveContext<'a> {
    /// 我们自己的存储（`installed_versions` 从它里面读）。
    pub store: &'a tuoen_store::Store,
    /// 检测到的工具（只读）。
    pub detected: &'a [DetectedTool],
    /// 认识哪些工具（`KNOWN_TOOLS`）。
    pub known_tools: &'a [ToolSpec],
}

/// 解析成功的一条。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTool {
    /// 工具 id（小写）。
    pub name: String,
    /// 声明时的规格原文。
    pub spec: String,
    /// 落到的确定版本。
    pub version: String,
    /// 来源：`tuoen` 或检测来源的 kebab slug（`path-resolution` / `scoop`… 见
    /// [`DetectionSource::as_str`]）。
    pub source: String,
    /// 第三方管理器名（`nvm4w` / `scoop`…）；我们自己装的没有。
    pub manager: Option<String>,
    /// **要前置进子进程 `PATH` 的目录**（不是可执行文件的全路径）。
    pub path: PathBuf,
    /// 制品哈希 `sha256:<hex>`；只能从我们自己的安装记录里拿到。
    pub hash: Option<String>,
}

/// 声明了、但机器上没有任何版本满足它。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingTool {
    /// 工具 id。
    pub name: String,
    /// 声明的规格原文。
    pub spec: String,
    /// 这台机器上已有的版本（去重、按版本降序）；一个都没有时为空。
    pub installed: Vec<String>,
    /// 该跑的那条命令，形如 `tuoen install node@24`。
    pub next_command: String,
}

/// 解析的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// 全部解析出来了（顺序 = 工具 id 字典序）。
    All(Vec<ResolvedTool>),
    /// 有缺口：`missing` 是"没装"，`unknown` 是"不认识这个 id"。
    ///
    /// **部分成功也走这里**：`All` 的语义是"这份声明可以用了"，
    /// 而少一个工具的 `PATH` 不是"可以用了"。
    Failed(Vec<MissingTool>, Vec<String>),
}

/// 一个候选（内部用：来源、版本、前置目录、哈希、优先级）。
struct Candidate {
    version: String,
    source: String,
    manager: Option<String>,
    path: PathBuf,
    hash: Option<String>,
    /// 来源的优先级：0 = 我们自己装的（store 记录），1 = tuoen 但来自检测，2 = 第三方。
    rank: u8,
    /// 是不是 `tuoen`（同版本时的第一顺位，决策 114）。
    from_tuoen: bool,
}

/// 解析一份声明。
#[must_use]
pub fn resolve(ctx: &ResolveContext<'_>, pin: &PinFile) -> Resolution {
    let mut unknown = Vec::new();
    let mut resolved = Vec::new();
    let mut missing = Vec::new();

    for (name, spec) in pin.tools() {
        if !ctx
            .known_tools
            .iter()
            .any(|known| known.id.eq_ignore_ascii_case(name))
        {
            unknown.push(name.clone());
            continue;
        }
        let candidates = candidates_for(ctx, name);
        let mut matching: Vec<&Candidate> = candidates
            .iter()
            .filter(|candidate| spec.matches(&candidate.version))
            .collect();
        if matching.is_empty() {
            missing.push(MissingTool {
                name: name.clone(),
                spec: spec.raw().to_owned(),
                installed: known_versions(&candidates),
                next_command: format!("tuoen install {name}@{}", spec.raw()),
            });
            continue;
        }
        matching.sort_by(|left, right| best_first(left, right));
        let best = matching[0];
        resolved.push(ResolvedTool {
            name: name.clone(),
            spec: spec.raw().to_owned(),
            version: best.version.clone(),
            source: best.source.clone(),
            manager: best.manager.clone(),
            path: best.path.clone(),
            hash: best.hash.clone(),
        });
    }

    if missing.is_empty() && unknown.is_empty() {
        Resolution::All(resolved)
    } else {
        Resolution::Failed(missing, unknown)
    }
}

/// 挑"最好"的那个：版本大的优先，同版本 `tuoen` 优先，再平手就按来源名 / 路径定序。
fn best_first(left: &Candidate, right: &Candidate) -> Ordering {
    tuoen_store::compare_versions(&right.version, &left.version)
        .then_with(|| right.from_tuoen.cmp(&left.from_tuoen))
        .then_with(|| left.rank.cmp(&right.rank))
        .then_with(|| left.source.cmp(&right.source))
        .then_with(|| left.path.cmp(&right.path))
}

/// 一台机器上已知的版本号（去重、降序）—— 缺口消息里要能回答"那都装了些什么"。
fn known_versions(candidates: &[Candidate]) -> Vec<String> {
    let mut versions: Vec<String> = candidates
        .iter()
        .map(|candidate| candidate.version.clone())
        .collect();
    versions.sort_by(|left, right| tuoen_store::compare_versions(right, left));
    versions.dedup();
    versions
}

/// 收集一个工具的全部候选。
fn candidates_for(ctx: &ResolveContext<'_>, name: &str) -> Vec<Candidate> {
    let mut out = Vec::new();

    for installed in tuoen_store::installed_versions(ctx.store, name) {
        let (path, hash) = match &installed.record {
            Some(record) => (
                our_bin_dir(&installed.path, &record.layout),
                Some(format!("sha256:{}", record.sha256)),
            ),
            // 没有记录（记录写失败、或者被手删了）：载荷目录仍然是事实
            // （store 的文档：目录才是事实来源）。保守地前置版本目录**本身** ——
            // 猜一个 `bin` 出来会让 `PATH` 指向一个不存在的目录。
            None => (tidy(&installed.path), None),
        };
        out.push(Candidate {
            version: installed.version.clone(),
            source: "tuoen".to_owned(),
            manager: None,
            path,
            hash,
            rank: 0,
            from_tuoen: true,
        });
    }

    for tool in ctx.detected {
        if !tool.name.eq_ignore_ascii_case(name) {
            continue;
        }
        let Some(version) = tool.version.as_deref() else {
            // 发现但不知道版本：它没法回答"满足不满足规格"这个问题。
            continue;
        };
        let version = version.trim();
        if version.is_empty() {
            continue;
        }
        // 没有路径的检测结果没法前置 —— 跳过它，而不是生成一条"前置空目录"的候选
        // （空条目进 `PATH` 会让当前目录变成命令来源）。
        if tool.path.trim().is_empty() {
            continue;
        }
        let from_tuoen = tool.source == DetectionSource::Tuoen;
        out.push(Candidate {
            version: version.to_owned(),
            source: tool.source.as_str().to_owned(),
            manager: tool.manager.clone(),
            path: prepend_dir_of(&tool.path),
            hash: None,
            rank: if from_tuoen { 1 } else { 2 },
            from_tuoen,
        });
    }

    out
}

/// 我们自己装的版本：从解压布局里算出"该前置哪个目录"。
fn our_bin_dir(version_dir: &Path, layout: &tuoen_manifest::Layout) -> PathBuf {
    // `bin` 是 `BTreeMap`：第一条是**命令名字典序**最小的那条。
    // 同一个版本里的各条命令通常同处一个目录，所以"第一条"够用；
    // 真要分成两个目录（`bin/` 与 `sbin/`），那是另一票的事。
    if let Some((_, relative)) = layout.bin.iter().next() {
        let executable = version_dir.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = executable.parent()
            && !parent.as_os_str().is_empty()
        {
            return tidy(parent);
        }
        return tidy(version_dir);
    }
    if let Some(home) = layout.home.as_deref() {
        let home = home.trim();
        if !home.is_empty() {
            return tidy(&version_dir.join(home.replace('/', std::path::MAIN_SEPARATOR_STR)));
        }
    }
    tidy(version_dir)
}

/// 第三方候选：`DetectedTool::path` 可能是**可执行文件**，也可能是**安装根目录**
/// （注册表 ARP 给的就是目录）。
///
/// 判断顺序：**存在且真的是目录 → 原样**；否则按"末段含 `.`"猜它是文件、取父目录。
/// 为什么先问磁盘：`C:\Program Files\Java\jdk-17.0.1` 这种目录名带点，
/// 只按"含点即文件"判会把它截成 `C:\Program Files\Java` ——
/// 那是把用户送到一个**别的**目录里去，比"猜不出"糟得多。
/// 磁盘问答不改变任何东西（只读），而"存在"是这里能拿到的最强证据。
fn prepend_dir_of(detected_path: &str) -> PathBuf {
    let raw = PathBuf::from(detected_path);
    if raw.as_os_str().is_empty() {
        return raw;
    }
    if raw.is_dir() {
        return tidy(&absolute_dir(&raw));
    }
    let looks_like_file = raw
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains('.'));
    if !looks_like_file {
        return tidy(&absolute_dir(&raw));
    }
    match raw.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => tidy(&absolute_dir(parent)),
        // 单段的相对路径（`node.exe`）：它所在的那个目录就是当前目录。
        _ => PathBuf::from("."),
    }
}

/// 去掉 `.` 成分（`join(".")` 会留下它）。纯词法，不碰文件系统。
fn tidy(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        if matches!(component, std::path::Component::CurDir) {
            continue;
        }
        out.push(component);
    }
    if out.as_os_str().is_empty() {
        PathBuf::from(".")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::KNOWN_TOOLS;
    use crate::pin::test_support::{FakeInstall, TempDir, detected_tool};
    use crate::pin::{PIN_FILE_NAME, PinFile};
    use tuoen_store::Store;

    fn pin_of(tools: &[(&str, &str)]) -> PinFile {
        let mut text = String::from("[tools]\n");
        for (id, spec) in tools {
            text.push_str(&format!("{id} = \"{spec}\"\n"));
        }
        PinFile::parse(&text).expect("测试用的声明应当合法")
    }

    fn resolve_with(root: &Path, detected: &[DetectedTool], pin: &PinFile) -> Resolution {
        let store = Store::new(root);
        let ctx = ResolveContext {
            store: &store,
            detected,
            known_tools: KNOWN_TOOLS,
        };
        resolve(&ctx, pin)
    }

    #[test]
    fn our_own_install_resolves_to_the_bin_dir_from_the_layout() {
        let temp = TempDir::new("resolve-store");
        let version_dir = FakeInstall::new("node", "24.19.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let resolution = resolve_with(temp.path(), &[], &pin_of(&[("node", "24")]));
        let Resolution::All(tools) = resolution else {
            panic!("应当解析成功：{resolution:?}");
        };
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "node");
        assert_eq!(tools[0].version, "24.19.0");
        assert_eq!(tools[0].source, "tuoen");
        assert_eq!(tools[0].manager, None);
        assert_eq!(tools[0].path, version_dir.join("bin"));
        assert_eq!(tools[0].path, tidy(&version_dir.join("bin")));
        assert_eq!(
            tools[0].hash.as_deref(),
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000001")
        );
        assert!(tools[0].path.is_dir(), "前置目录必须真的存在");
    }

    #[test]
    fn layout_without_bin_falls_back_to_home_then_to_the_version_dir() {
        // `bin` 空 → `home`。
        let temp = TempDir::new("resolve-home");
        let version_dir = FakeInstall::new("java", "21.0.2")
            .file("bin/java.exe")
            .home(".")
            .install(temp.path());
        let Resolution::All(tools) = resolve_with(temp.path(), &[], &pin_of(&[("java", "21")]))
        else {
            panic!("应当解析成功");
        };
        assert_eq!(tools[0].path, tidy(&version_dir));

        // `bin` 与 `home` 都空 → 版本目录本身。
        let temp = TempDir::new("resolve-bare");
        let version_dir = FakeInstall::new("go", "1.23.4")
            .file("README")
            .install(temp.path());
        let Resolution::All(tools) = resolve_with(temp.path(), &[], &pin_of(&[("go", "1.23")]))
        else {
            panic!("应当解析成功");
        };
        assert_eq!(tools[0].path, tidy(&version_dir));
        assert!(tools[0].hash.is_some());
    }

    #[test]
    fn higher_matching_version_wins_across_sources() {
        // 决策 114 的字面：同版本时 tuoen 优先；不同版本时**大的那个**赢，哪怕它不是我们装的。
        let temp = TempDir::new("resolve-max");
        FakeInstall::new("node", "24.15.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let detected = vec![detected_tool(
            "node",
            Some("24.19.0"),
            "C:\\Program Files\\nodejs\\node.exe",
            DetectionSource::PathResolution,
        )];
        let Resolution::All(tools) =
            resolve_with(temp.path(), &detected, &pin_of(&[("node", "24")]))
        else {
            panic!("应当解析成功");
        };
        assert_eq!(tools[0].version, "24.19.0");
        assert_eq!(tools[0].source, "path-resolution");
        assert_eq!(tools[0].hash, None);
    }

    #[test]
    fn same_version_prefers_our_own_install() {
        let temp = TempDir::new("resolve-tie");
        let version_dir = FakeInstall::new("node", "24.19.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let detected = vec![detected_tool(
            "node",
            Some("24.19.0"),
            "C:\\Program Files\\nodejs\\node.exe",
            DetectionSource::PathResolution,
        )];
        let Resolution::All(tools) =
            resolve_with(temp.path(), &detected, &pin_of(&[("node", "24")]))
        else {
            panic!("应当解析成功");
        };
        assert_eq!(tools[0].source, "tuoen");
        assert_eq!(tools[0].path, version_dir.join("bin"));
        assert!(tools[0].hash.is_some());
    }

    #[test]
    fn detected_path_that_is_a_file_takes_its_parent() {
        let temp = TempDir::new("resolve-detected-file");
        // 造一个真的"文件在磁盘上"的形状：`<dir>\node.exe`。
        let dir = temp.mkdir("Program Files/nodejs");
        temp.write("Program Files/nodejs/node.exe", b"");
        let detected = vec![detected_tool(
            "node",
            Some("24.19.0"),
            &dir.join("node.exe").to_string_lossy(),
            DetectionSource::PathResolution,
        )];
        let Resolution::All(tools) =
            resolve_with(temp.path(), &detected, &pin_of(&[("node", "24")]))
        else {
            panic!("应当解析成功");
        };
        assert_eq!(tools[0].path, tidy(&dir));
    }

    #[test]
    fn detected_directory_with_a_dot_in_its_name_is_kept() {
        // 注册表 ARP 给的常常就是目录，而目录名里带点的很多（`jdk-17.0.1`）。
        let temp = TempDir::new("resolve-detected-dir");
        let dir = temp.mkdir("Java/jdk-17.0.1");
        let detected = vec![detected_tool(
            "java",
            Some("17.0.1"),
            &dir.to_string_lossy(),
            DetectionSource::RegistryArp,
        )];
        let Resolution::All(tools) =
            resolve_with(temp.path(), &detected, &pin_of(&[("java", "17")]))
        else {
            panic!("应当解析成功");
        };
        assert_eq!(tools[0].path, tidy(&dir), "存在的目录原样保留");
        assert_eq!(tools[0].source, "registry-arp");
    }

    #[test]
    fn detected_without_a_path_is_ignored() {
        // 没有路径的检测结果没法前置：跳过它，而不是造一条"前置空目录"的候选
        // （空条目进 PATH 会让当前目录变成命令来源）。
        let temp = TempDir::new("resolve-no-path");
        let detected = vec![detected_tool(
            "node",
            Some("24.19.0"),
            "   ",
            DetectionSource::PathResolution,
        )];
        let resolution = resolve_with(temp.path(), &detected, &pin_of(&[("node", "24")]));
        match resolution {
            Resolution::Failed(missing, _) => {
                assert_eq!(missing.len(), 1);
                assert!(missing[0].installed.is_empty(), "{:?}", missing[0]);
            }
            other => panic!("应当失败：{other:?}"),
        }
    }

    #[test]
    fn detected_without_a_version_is_ignored() {
        let temp = TempDir::new("resolve-no-version");
        let detected = vec![detected_tool(
            "node",
            None,
            "C:\\Program Files\\nodejs\\node.exe",
            DetectionSource::PathResolution,
        )];
        let resolution = resolve_with(temp.path(), &detected, &pin_of(&[("node", "24")]));
        match resolution {
            Resolution::Failed(missing, unknown) => {
                assert_eq!(missing.len(), 1);
                assert!(unknown.is_empty());
                // 版本未知的候选不进 `installed`：它不是"一个已装的版本"。
                assert!(missing[0].installed.is_empty(), "{:?}", missing[0]);
            }
            other => panic!("应当失败：{other:?}"),
        }
    }

    #[test]
    fn missing_version_reports_installed_versions_and_the_next_command() {
        let temp = TempDir::new("resolve-missing");
        FakeInstall::new("node", "20.11.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        FakeInstall::new("node", "18.20.4")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let resolution = resolve_with(temp.path(), &[], &pin_of(&[("node", "24")]));
        match resolution {
            Resolution::Failed(missing, unknown) => {
                assert!(unknown.is_empty());
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].name, "node");
                assert_eq!(missing[0].spec, "24");
                assert_eq!(
                    missing[0].installed,
                    vec!["20.11.0".to_owned(), "18.20.4".to_owned()],
                    "已装的版本按降序"
                );
                assert_eq!(missing[0].next_command, "tuoen install node@24");
                let error = crate::pin::PinError::version_not_installed(&missing[0]);
                assert_eq!(error.code(), "version-not-installed");
                assert!(error.message().contains("20.11.0"), "{}", error.message());
            }
            other => panic!("应当失败：{other:?}"),
        }
    }

    #[test]
    fn unknown_tool_id_is_reported_separately() {
        let temp = TempDir::new("resolve-unknown");
        let resolution = resolve_with(temp.path(), &[], &pin_of(&[("nod", "24"), ("node", "24")]));
        match resolution {
            Resolution::Failed(missing, unknown) => {
                assert_eq!(unknown, vec!["nod".to_owned()]);
                assert_eq!(missing.len(), 1, "{missing:?}");
                assert_eq!(missing[0].name, "node");
            }
            other => panic!("应当失败：{other:?}"),
        }
    }

    #[test]
    fn partial_success_is_still_a_failure() {
        // 一个装好了、一个没装：`All` 的语义是"这份声明可以用了"，所以这里必须是 Failed。
        let temp = TempDir::new("resolve-partial");
        FakeInstall::new("node", "24.19.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let resolution = resolve_with(
            temp.path(),
            &[],
            &pin_of(&[("node", "24"), ("python", "3.12")]),
        );
        match resolution {
            Resolution::Failed(missing, unknown) => {
                assert!(unknown.is_empty());
                assert_eq!(missing.len(), 1);
                assert_eq!(missing[0].name, "python");
                assert_eq!(missing[0].next_command, "tuoen install python@3.12");
            }
            other => panic!("应当失败：{other:?}"),
        }
    }

    #[test]
    fn all_returns_tools_in_id_order() {
        let temp = TempDir::new("resolve-order");
        FakeInstall::new("python", "3.12.7")
            .command("python", "python.exe")
            .install(temp.path());
        FakeInstall::new("node", "24.19.0")
            .command("node", "bin/node.exe")
            .install(temp.path());
        let Resolution::All(tools) = resolve_with(
            temp.path(),
            &[],
            &pin_of(&[("python", "3.12"), ("node", "24")]),
        ) else {
            panic!("应当解析成功");
        };
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, vec!["node", "python"], "id 字典序");
    }

    #[test]
    fn empty_declaration_resolves_to_nothing() {
        let temp = TempDir::new("resolve-empty");
        let Resolution::All(tools) = resolve_with(temp.path(), &[], &pin_of(&[])) else {
            panic!("空声明应当成功解析成空");
        };
        assert!(tools.is_empty());
    }

    #[test]
    fn install_without_a_record_still_points_at_the_version_dir() {
        let temp = TempDir::new("resolve-no-record");
        // 手造一个只有目录、没有 `<version>.json` 的"已装版本"（store 的文档：目录才是事实）。
        temp.mkdir("node/versions/24.19.0/bin");
        let Resolution::All(tools) = resolve_with(temp.path(), &[], &pin_of(&[("node", "24")]))
        else {
            panic!("应当解析成功（记录不是事实来源）");
        };
        assert_eq!(tools[0].hash, None);
        assert_eq!(
            tools[0].path,
            tidy(&temp.join("node/versions/24.19.0")),
            "没有记录时前置版本目录本身，而不是猜一个 bin"
        );
    }

    #[test]
    fn pin_file_and_bin_dir_paths_are_used_together() {
        // 顺带钉一下：`resolve` 不读 `tuoen.toml` 的位置，它只吃解析好的 `PinFile`。
        let temp = TempDir::new("resolve-pinfile");
        let pin_path = temp.write_text(PIN_FILE_NAME, "[tools]\nnode = \"24\"\n");
        let pin = PinFile::load(&pin_path).expect("读声明");
        let Resolution::Failed(missing, _) = resolve_with(temp.path(), &[], &pin) else {
            panic!("没装，应当失败");
        };
        assert_eq!(missing[0].installed.len(), 0);
    }
}
