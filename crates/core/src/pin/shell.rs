//! 子 shell 的环境计划（决策 119–121）。
//!
//! # 计划是什么
//!
//! [`ShellPlan`] 是**纯数据**：cwd、要前置哪些目录、`PATH` 的前后两份、遮蔽警告、深度。
//! 它不启动任何进程（[`ShellLauncher`] 才是那件事的接口，而且这里不实现它），
//! 也不改父进程的环境 —— 决策 119 的原话是"不翻转任何 junction、不改父进程环境"。
//!
//! # `PATH` 的两份
//!
//! * `path_before`：调用方给进来的那份，**原样**（一个字都不动）。
//! * `path_after`：前置目录（按工具 id **字典序**）在前，其余条目按原顺序跟在后面，
//!   且**与前置目录重复的条目被去掉**。
//!
//! 去重的比较是"忽略大小写 + 忽略尾部分隔符"（复用 [`tuoen_platform::normalize_entry`]，
//! 与 `PATH` 分析那一处同一套判据）。**不做绝对化**：一条相对的 `PATH` 条目是相对
//! **子进程**的工作目录解析的，而那个目录正是我们这一票要设的东西 ——
//! 拿父进程的 cwd 去绝对化会算出一个不成立的比较。
//!
//! # 警告是"前置了却没生效"（决策 120）
//!
//! 判据不是"有没有重名"，而是**拿 `path_after` 的顺序解析一遍**：
//! 每个被 pin 的工具的每个命令名，谁第一个被命中？
//!
//! | 命中位置 | 结论 |
//! |---|---|
//! | 前置目录里 | 没话说（不报） |
//! | 后面的某个目录 | **被它压住了** → [`ShadowWarning`]，`winner` 是那个目录 |
//! | 整条 `PATH` 都没有 | `winner = None` —— 布局猜错、版本目录选错，或者命令名不对 |
//!
//! 最后一态是这一票真正要抓的东西：**用户以为切了版本，其实什么都没切**。
//! 它比"被压住"更糟，所以它单独可辨（`None` 而不是空串）。
//!
//! "一个目录里有没有这个命令"的判据与 [`tuoen_platform::path::detect_shadowing`] 同一套
//! （目录名转小写 + 逐个拼 [`tuoen_platform::PATH_EXTENSIONS`]）。它**不能直接复用**
//! 那个函数：那个函数的分母是"我们自己的 shim 目录里实际存在的 `*.exe`"，
//! 而这里的分母是"工具规格声明的命令名"，要问的是"某个目录里有没有这条命令"。
//! 形状只有下面 [`dir_has_command`] 那几行 —— 判据要改，两处一起改。
//!
//! # 深度
//!
//! `TUOEN_SHELL_DEPTH` 记录"这是第几层子 shell"（决策 121）。它由**启动子进程的那一方**
//! 写成 `plan.depth() + 1`，读进来时用 [`depth_from_env`] —— 坏值当 0
//! （宁可让用户多开一层，也不要因为一个读不懂的环境变量把人锁在门外）。

use std::path::{Path, PathBuf};

use crate::detect::ToolSpec;
use crate::pin::PinError;
use crate::pin::resolve::ResolvedTool;
use tuoen_platform::{FileSystem, PATH_EXTENSIONS};

/// 记录子 shell 层数的环境变量名。
pub const SHELL_DEPTH_VAR: &str = "TUOEN_SHELL_DEPTH";

/// 允许的最大层数（决策 121：`depth > MAX_SHELL_DEPTH` 就拒绝启动）。
pub const MAX_SHELL_DEPTH: u32 = 5;

/// 读环境变量里的深度。**坏值（空串、负数、非数字、超范围）当 0。**
///
/// 0 的含义是"不是从 `tuoen shell` 进来的"，那是一个诚实的默认值。
#[must_use]
pub fn depth_from_env(value: Option<&str>) -> u32 {
    value
        .and_then(|text| text.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// 进哪一个 shell。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellKind {
    /// `cmd.exe`。
    Cmd,
    /// PowerShell。
    PowerShell,
}

impl ShellKind {
    /// `--json` 里的稳定字符串，**不本地化**。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cmd => "cmd",
            Self::PowerShell => "powershell",
        }
    }
}

impl std::fmt::Display for ShellKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 启动子 shell 的规格。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    /// 哪一种 shell。
    pub kind: ShellKind,
    /// `--exec`：启动后要跑的那一串（原样交给 shell，不由我们解析）。
    pub exec: Option<String>,
}

impl ShellSpec {
    /// 只要一个交互式 shell。
    #[must_use]
    pub fn new(kind: ShellKind) -> Self {
        Self { kind, exec: None }
    }

    /// 带上要执行的命令。
    #[must_use]
    pub fn with_exec(mut self, exec: impl Into<String>) -> Self {
        self.exec = Some(exec.into());
        self
    }
}

/// 一条"前置了却没生效"的警告。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowWarning {
    /// 命令名（`node` / `npm`…）。**不是**文件名（`node.exe`）。
    pub command: String,
    /// 赢得这个名字冲突的**目录**（归一化后的文本）。
    ///
    /// `None` = 整条 `PATH` 里**没有任何目录**有这个命令 —— 见模块文档最后一态。
    pub winner: Option<String>,
    /// 那个目录在 `path_after` 里的**原文**（可能带尾部分隔符或引号），
    /// 让人一眼能对回自己 `PATH` 里的那一条。`winner` 是 `None` 时它也是 `None`。
    pub entry: Option<String>,
}

/// 已经算好的一份子 shell 环境计划。
#[derive(Debug, Clone)]
pub struct ShellPlan {
    cwd: PathBuf,
    tools: Vec<ResolvedTool>,
    prepend: Vec<PathBuf>,
    path_before: String,
    path_after: String,
    warnings: Vec<ShadowWarning>,
    depth: u32,
}

/// `path_after` 里的一条，带上"它是不是我们前置进去的"。
struct PathItem {
    raw: String,
    value: String,
    prepended: bool,
}

impl ShellPlan {
    /// 用真实文件系统算一份计划。
    #[must_use]
    pub fn build(
        cwd: &Path,
        tools: &[ResolvedTool],
        process_path: &str,
        known_tools: &[ToolSpec],
        depth: u32,
    ) -> ShellPlan {
        Self::build_with(
            &tuoen_platform::RealFileSystem,
            cwd,
            tools,
            process_path,
            known_tools,
            depth,
        )
    }

    /// 用**注入的**文件系统算一份计划。
    ///
    /// 与 [`ShellPlan::build`] 的唯一区别是"哪个目录里有哪些文件"这个问题问谁 ——
    /// 于是测试可以在固定装置上断言遮蔽判定，而"计划"这件事本身不必碰真机。
    #[must_use]
    pub fn build_with<F: FileSystem + ?Sized>(
        fs: &F,
        cwd: &Path,
        tools: &[ResolvedTool],
        process_path: &str,
        known_tools: &[ToolSpec],
        depth: u32,
    ) -> ShellPlan {
        let tools = sorted_tools(tools);
        let prepend = prepend_dirs(&tools);
        let items = path_items(&prepend, process_path);
        let path_after = items
            .iter()
            .map(|item| item.raw.as_str())
            .collect::<Vec<_>>()
            .join(";");
        let warnings = warnings_for(fs, &tools, &items, known_tools);
        Self {
            cwd: cwd.to_path_buf(),
            tools,
            prepend,
            path_before: process_path.to_owned(),
            path_after,
            warnings,
            depth,
        }
    }

    /// 子进程的工作目录（项目根）。
    #[must_use]
    pub fn cwd(&self) -> &Path {
        &self.cwd
    }

    /// 解析出来的工具（顺序 = 工具 id 字典序）。
    #[must_use]
    pub fn tools(&self) -> &[ResolvedTool] {
        &self.tools
    }

    /// 要前置的目录（顺序 = 工具 id 字典序，已去重）。
    #[must_use]
    pub fn prepend(&self) -> &[PathBuf] {
        &self.prepend
    }

    /// 原来的 `PATH`，原样。
    #[must_use]
    pub fn path_before(&self) -> &str {
        &self.path_before
    }

    /// 要给子进程的 `PATH`。
    #[must_use]
    pub fn path_after(&self) -> &str {
        &self.path_after
    }

    /// 遮蔽警告（没有问题时是空表，**不是**"没查"）。
    #[must_use]
    pub fn warnings(&self) -> &[ShadowWarning] {
        &self.warnings
    }

    /// 这一层的深度。
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth
    }

    /// 已经太深了，不该再开一层（`depth > MAX_SHELL_DEPTH`，决策 121）。
    #[must_use]
    pub fn too_deep(&self) -> bool {
        self.depth > MAX_SHELL_DEPTH
    }
}

/// 启动子 shell。
///
/// **core 里没有任何实现，也没有任何东西会调用它。** 它的存在是把"子 shell 是可注入的"
/// 这件事留在类型上：真正的启动在 CLI 侧，而那边"构造命令行"只能是**一个**函数
/// （`AGENTS.md` 铁律 2：两次进程事故都出在"命令行有多个构造点"上）。
/// 这里的签名是那个函数的形状，不是它的替身。
pub trait ShellLauncher {
    /// 启动 `spec` 描述的 shell，让它在 `plan` 的环境里跑，返回它的退出码。
    ///
    /// # Errors
    ///
    /// 启动失败、或者这一层的深度已经越界 → [`PinError`]。
    fn launch(&self, plan: &ShellPlan, spec: &ShellSpec) -> Result<i32, PinError>;
}

/// 按工具 id 字典序排。顺序不是装饰：它是 `PATH` 前置顺序，也是报告顺序 ——
/// 两次运行给出两种顺序会让"哪一条是新的"这个问题没法回答。
fn sorted_tools(tools: &[ResolvedTool]) -> Vec<ResolvedTool> {
    let mut sorted = tools.to_vec();
    sorted.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.version.cmp(&right.version))
    });
    sorted
}

/// 前置目录：去重（忽略大小写 + 忽略尾部分隔符），丢掉空的。
fn prepend_dirs(tools: &[ResolvedTool]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for tool in tools {
        let text = tool.path.to_string_lossy().into_owned();
        let value = entry_key(&text);
        if value.is_empty() {
            // 空的前置目录进 `PATH` 只会让"当前目录"变成命令来源。
            continue;
        }
        if keys.contains(&value) {
            continue;
        }
        keys.push(value);
        out.push(tool.path.clone());
    }
    out
}

/// 条目的比较键：去尾部分隔符 + 转小写。
fn entry_key(text: &str) -> String {
    tuoen_platform::normalize_entry(text).to_ascii_lowercase()
}

/// 拼出 `path_after` 的条目序列。
fn path_items(prepend: &[PathBuf], path_before: &str) -> Vec<PathItem> {
    let mut items: Vec<PathItem> = prepend
        .iter()
        .map(|path| {
            let text = path.to_string_lossy().into_owned();
            PathItem {
                value: text.clone(),
                raw: text,
                prepended: true,
            }
        })
        .collect();
    let prepended_keys: Vec<String> = prepend
        .iter()
        .map(|path| entry_key(&path.to_string_lossy()))
        .collect();
    for entry in tuoen_platform::parse_entries(path_before) {
        let key = entry_key(&entry.value);
        if key.is_empty() {
            // 空条目（`;;`）在 Windows 上的含义没有定义，留着只会让 `path_after`
            // 与原串多一处不必要的不同。
            continue;
        }
        if prepended_keys.contains(&key) {
            continue;
        }
        items.push(PathItem {
            raw: entry.raw,
            value: entry.value,
            prepended: false,
        });
    }
    items
}

/// "前置了却没生效"的判定（决策 120）。
fn warnings_for<F: FileSystem + ?Sized>(
    fs: &F,
    tools: &[ResolvedTool],
    items: &[PathItem],
    known_tools: &[ToolSpec],
) -> Vec<ShadowWarning> {
    let mut warnings = Vec::new();
    let mut handled: Vec<String> = Vec::new();
    for tool in tools {
        for command in command_names_for(&tool.name, known_tools) {
            let key = command.to_ascii_lowercase();
            if handled.contains(&key) {
                // 同一条命令被两个工具声明时只报一次：报两次会让人以为是两件事。
                continue;
            }
            handled.push(key);
            match items
                .iter()
                .find(|item| dir_has_command(fs, Path::new(&item.value), &command))
            {
                Some(item) if item.prepended => {}
                Some(item) => warnings.push(ShadowWarning {
                    command,
                    winner: Some(item.value.clone()),
                    entry: Some(item.raw.clone()),
                }),
                None => warnings.push(ShadowWarning {
                    command,
                    winner: None,
                    entry: None,
                }),
            }
        }
    }
    warnings
}

/// 一个工具要在 `PATH` 上暴露的命令名。
///
/// 两处合起来才是完整的：`shim` 那张表说的是"我们会为它生成哪些 shim"
/// （目前只有 node，而且是最权威的一条 —— 它决定用户实际会敲什么），
/// 工具规格里的 `ExecutableName::command` 覆盖剩下的工具。
fn command_names_for(tool: &str, known_tools: &[ToolSpec]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in crate::shim::command_names(tool) {
        add_unique_command(&mut out, name);
    }
    if let Some(spec) = known_tools
        .iter()
        .find(|spec| spec.id.eq_ignore_ascii_case(tool))
    {
        for executable in spec.executables {
            add_unique_command(&mut out, executable.command);
        }
    }
    out
}

fn add_unique_command(out: &mut Vec<String>, name: &str) {
    if out
        .iter()
        .any(|existing| existing.eq_ignore_ascii_case(name))
    {
        return;
    }
    out.push(name.to_owned());
}

/// 这个目录里有这条命令吗。
///
/// 判据与 [`tuoen_platform::path::detect_shadowing`] 同一套：目录名转小写，
/// 逐个拼 [`PATH_EXTENSIONS`]。多出来的那一行是"名字本身就是 `node`（没有扩展名）"——
/// `cmd.exe` 真的会尝试执行这种文件，所以它也算命中。
fn dir_has_command<F: FileSystem + ?Sized>(fs: &F, dir: &Path, command: &str) -> bool {
    let names: Vec<String> = fs
        .list_dir(dir)
        .into_iter()
        .map(|entry| entry.name.to_ascii_lowercase())
        .collect();
    if names.is_empty() {
        return false;
    }
    let command = command.to_ascii_lowercase();
    if names.contains(&command) {
        return true;
    }
    PATH_EXTENSIONS.iter().any(|extension| {
        let file = format!("{command}.{extension}");
        names.contains(&file)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::KNOWN_TOOLS;
    use crate::pin::test_support::{FakeDirs, TempDir, resolved_tool};

    fn node_tool(path: &Path) -> ResolvedTool {
        resolved_tool("node", "24", "24.19.0", path)
    }

    fn python_tool(path: &Path) -> ResolvedTool {
        resolved_tool("python", "3.12", "3.12.7", path)
    }

    fn build(cwd: &Path, tools: &[ResolvedTool], path: &str) -> ShellPlan {
        ShellPlan::build(cwd, tools, path, KNOWN_TOOLS, 0)
    }

    #[test]
    fn prepend_order_is_lexicographic_by_tool_id() {
        let temp = TempDir::new("shell-order");
        let node_dir = temp.mkdir("node-bin");
        let python_dir = temp.mkdir("py-bin");
        // 故意把 python 放在前面：顺序必须由 id 决定，不由调用方给的顺序决定。
        let plan = build(
            temp.path(),
            &[python_tool(&python_dir), node_tool(&node_dir)],
            "C:\\Windows\\System32",
        );
        assert_eq!(plan.prepend(), &[node_dir, python_dir]);
        assert!(
            plan.path_after()
                .starts_with(&format!("{};", plan.prepend()[0].display()))
        );
        assert_eq!(plan.tools()[0].name, "node");
        assert_eq!(plan.tools()[1].name, "python");
    }

    #[test]
    fn path_after_prepends_and_dedupes() {
        let temp = TempDir::new("shell-dedupe");
        let node_dir = temp.mkdir("node-bin");
        let node_text = node_dir.to_string_lossy().into_owned();
        let path_before = format!("C:\\Windows\\System32;{node_text}\\;D:\\other");
        let plan = build(temp.path(), &[node_tool(&node_dir)], &path_before);
        let expected = format!("{node_text};C:\\Windows\\System32;D:\\other");
        assert_eq!(plan.path_after(), expected);
        // 去重是"忽略尾部分隔符"的：上面那条带 `\` 的重复条目被吃掉了。
        assert_eq!(plan.path_after().matches(';').count(), 2);
        // `path_before` 一个字都不动。
        assert_eq!(plan.path_before(), path_before);
        // 这个固定目录里没有任何命令文件，所以四条命令都会各报一条警告 ——
        // 那是**判定正确**（决策 120 的最后一态），不是这里的失败。
        assert!(!plan.warnings().is_empty(), "空目录不该被当成「已生效」");
    }

    #[test]
    fn dedupe_ignores_case() {
        let temp = TempDir::new("shell-case");
        let dir = temp.mkdir("Node-Bin");
        let shouty = dir.to_string_lossy().to_uppercase();
        let path_before = format!("{shouty};D:\\keep");
        let plan = build(temp.path(), &[node_tool(&dir)], &path_before);
        assert_eq!(plan.path_after(), format!("{};D:\\keep", dir.display()));
    }

    #[test]
    fn empty_path_entries_are_dropped() {
        let temp = TempDir::new("shell-empty");
        let dir = temp.mkdir("bin");
        let plan = build(temp.path(), &[node_tool(&dir)], "C:\\a;;  ;C:\\b;");
        assert_eq!(plan.path_after(), format!("{};C:\\a;C:\\b", dir.display()));
        assert_eq!(plan.path_before(), "C:\\a;;  ;C:\\b;");
    }

    #[test]
    fn no_tools_means_path_is_only_the_original() {
        let temp = TempDir::new("shell-notools");
        let plan = build(temp.path(), &[], "C:\\a;C:\\b");
        assert!(plan.prepend().is_empty());
        assert_eq!(plan.path_after(), "C:\\a;C:\\b");
        assert_eq!(plan.cwd(), temp.path());
    }

    #[test]
    fn winner_inside_a_prepended_dir_is_not_a_warning() {
        let temp = TempDir::new("shell-ok");
        let dir = temp.mkdir("node-bin");
        temp.write("node-bin/node.cmd", b"");
        temp.write("node-bin/npm.cmd", b"");
        temp.write("node-bin/npx.cmd", b"");
        temp.write("node-bin/corepack.cmd", b"");
        let plan = build(temp.path(), &[node_tool(&dir)], "C:\\Windows\\System32");
        assert!(
            plan.warnings().is_empty(),
            "赢家在前置目录里，不该有警告：{:?}",
            plan.warnings()
        );
    }

    #[test]
    fn empty_prepended_dir_reports_every_command() {
        // 前置目录里一条命令都没有 → 每个命令名一条警告，且 `winner = None`
        // （整条 PATH 里都没有）—— 这是"以为切了版本、其实什么都没切"的样子。
        let temp = TempDir::new("shell-vacant");
        let dir = temp.mkdir("node-bin");
        let plan = build(temp.path(), &[node_tool(&dir)], "C:\\Windows\\System32");
        let commands: Vec<&str> = plan
            .warnings()
            .iter()
            .map(|warning| warning.command.as_str())
            .collect();
        assert!(commands.contains(&"node"), "{commands:?}");
        assert!(commands.contains(&"npm"), "{commands:?}");
        assert!(commands.contains(&"corepack"), "{commands:?}");
        for warning in plan.warnings() {
            assert_eq!(warning.winner, None, "{warning:?}");
            assert_eq!(warning.entry, None, "{warning:?}");
        }
    }

    #[test]
    fn shadowed_command_names_the_winner_and_its_raw_entry() {
        let temp = TempDir::new("shell-shadowed");
        let ours = temp.mkdir("node-bin");
        temp.write("node-bin/npm.cmd", b"");
        // 系统那份 node：赢家是它，而且它排在后面。
        let theirs = temp.mkdir("Program Files/nodejs");
        temp.write("Program Files/nodejs/node.exe", b"");
        let path_before = format!("{};C:\\Windows\\System32", theirs.display());
        let plan = build(temp.path(), &[node_tool(&ours)], &path_before);

        let node = plan
            .warnings()
            .iter()
            .find(|warning| warning.command == "node")
            .expect("node 应当被报成被压住");
        assert_eq!(
            node.winner.as_deref(),
            Some(tuoen_platform::normalize_entry(&theirs.to_string_lossy()).as_str())
        );
        assert_eq!(
            node.entry.as_deref(),
            Some(theirs.to_string_lossy().as_ref())
        );
        // npm 在我们前置的目录里 → 它不报。
        assert!(
            !plan
                .warnings()
                .iter()
                .any(|warning| warning.command == "npm"),
            "{:?}",
            plan.warnings()
        );
        // 前置目录仍然排在赢家前面（我们没改顺序，只是把事实报出来）。
        assert!(
            plan.path_after()
                .starts_with(&ours.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn bare_command_name_without_extension_counts() {
        let temp = TempDir::new("shell-bare");
        let ours = temp.mkdir("node-bin");
        let theirs = temp.mkdir("gnu/node");
        temp.write("gnu/node/node", b""); // 没有扩展名
        let plan = build(temp.path(), &[node_tool(&ours)], &theirs.to_string_lossy());
        let node = plan
            .warnings()
            .iter()
            .find(|warning| warning.command == "node")
            .expect("没有扩展名的命令也算命中");
        assert!(node.winner.is_some(), "{node:?}");
    }

    #[test]
    fn warnings_are_deterministic_and_ordered_by_tool_id() {
        let temp = TempDir::new("shell-order2");
        let node_dir = temp.mkdir("node-bin");
        let py_dir = temp.mkdir("py-bin");
        let plan = build(
            temp.path(),
            &[python_tool(&py_dir), node_tool(&node_dir)],
            "C:\\Windows\\System32",
        );
        let commands: Vec<&str> = plan
            .warnings()
            .iter()
            .map(|warning| warning.command.as_str())
            .collect();
        // node 的 4 条在 python 的 2 条（python / python3 / pip / pip3 见规格表）之前。
        assert!(commands.len() >= 4, "{commands:?}");
        assert_eq!(commands[0], "node");
        assert!(commands.contains(&"python"));
        let again = build(
            temp.path(),
            &[node_tool(&node_dir), python_tool(&py_dir)],
            "C:\\Windows\\System32",
        );
        let commands_again: Vec<&str> = again
            .warnings()
            .iter()
            .map(|warning| warning.command.as_str())
            .collect();
        assert_eq!(commands, commands_again, "顺序必须与调用方给的顺序无关");
    }

    #[test]
    fn injected_file_system_drives_the_shadow_judgement() {
        // 同一个 `PATH`、同一个工具，只换文件系统：结论相反。
        // 这条钉住"遮蔽判定确实来自注入的 FileSystem"，而不是来自真机。
        let ours = PathBuf::from("C:\\prepend\\node");
        let theirs = PathBuf::from("C:\\Program Files\\nodejs");
        let path_before = "C:\\Program Files\\nodejs;C:\\Windows";

        let empty = FakeDirs::new().with_dir(&ours, &[]).with_dir(&theirs, &[]);
        let plan = ShellPlan::build_with(
            &empty,
            Path::new("C:\\proj"),
            &[node_tool(&ours)],
            path_before,
            KNOWN_TOOLS,
            0,
        );
        let node = plan
            .warnings()
            .iter()
            .find(|warning| warning.command == "node")
            .expect("两边都没有 → winner = None 的那条警告");
        assert_eq!(node.winner, None);

        let with_theirs = FakeDirs::new()
            .with_dir(&ours, &["npm.cmd"])
            .with_dir(&theirs, &["node.exe"]);
        let plan = ShellPlan::build_with(
            &with_theirs,
            Path::new("C:\\proj"),
            &[node_tool(&ours)],
            path_before,
            KNOWN_TOOLS,
            0,
        );
        let node = plan
            .warnings()
            .iter()
            .find(|warning| warning.command == "node")
            .expect("别人的目录里有 node.exe → 被压住");
        assert!(node.winner.is_some(), "{node:?}");

        let with_ours = FakeDirs::new()
            .with_dir(&ours, &["node.exe"])
            .with_dir(&theirs, &["node.exe"]);
        let plan = ShellPlan::build_with(
            &with_ours,
            Path::new("C:\\proj"),
            &[node_tool(&ours)],
            path_before,
            KNOWN_TOOLS,
            0,
        );
        assert!(
            !plan
                .warnings()
                .iter()
                .any(|warning| warning.command == "node"),
            "我们赢了 → 不报：{:?}",
            plan.warnings()
        );
    }

    #[test]
    fn depth_comes_from_the_environment_with_a_forgiving_parser() {
        assert_eq!(depth_from_env(None), 0);
        assert_eq!(depth_from_env(Some("")), 0);
        assert_eq!(depth_from_env(Some("   ")), 0);
        assert_eq!(depth_from_env(Some("abc")), 0);
        assert_eq!(depth_from_env(Some("-1")), 0);
        assert_eq!(depth_from_env(Some("1.5")), 0);
        assert_eq!(depth_from_env(Some("4294967296")), 0);
        assert_eq!(depth_from_env(Some("0")), 0);
        assert_eq!(depth_from_env(Some(" 3 ")), 3);
        assert_eq!(depth_from_env(Some("5")), MAX_SHELL_DEPTH);
        assert_eq!(depth_from_env(Some("6")), 6);
    }

    #[test]
    fn too_deep_is_strictly_above_the_max() {
        let temp = TempDir::new("shell-depth");
        for depth in 0..=MAX_SHELL_DEPTH {
            let plan = ShellPlan::build(temp.path(), &[], "C:\\a", KNOWN_TOOLS, depth);
            assert!(!plan.too_deep(), "depth={depth} 不该算太深");
            assert_eq!(plan.depth(), depth);
        }
        for depth in [MAX_SHELL_DEPTH + 1, 99, u32::MAX] {
            let plan = ShellPlan::build(temp.path(), &[], "C:\\a", KNOWN_TOOLS, depth);
            assert!(plan.too_deep(), "depth={depth} 应当算太深");
        }
    }

    #[test]
    fn shell_kind_and_spec_strings_are_stable() {
        assert_eq!(ShellKind::Cmd.as_str(), "cmd");
        assert_eq!(ShellKind::PowerShell.as_str(), "powershell");
        assert_eq!(ShellKind::Cmd.to_string(), "cmd");
        let spec = ShellSpec::new(ShellKind::PowerShell).with_exec("node -v");
        assert_eq!(spec.kind, ShellKind::PowerShell);
        assert_eq!(spec.exec.as_deref(), Some("node -v"));
        assert_eq!(ShellSpec::new(ShellKind::Cmd).exec, None);
        assert_eq!(SHELL_DEPTH_VAR, "TUOEN_SHELL_DEPTH");
        assert_eq!(MAX_SHELL_DEPTH, 5);
    }

    #[test]
    fn duplicate_prepend_dirs_are_collapsed() {
        let temp = TempDir::new("shell-dupdirs");
        let dir = temp.mkdir("shared");
        // 两个工具落在同一个目录（比如同一套 SDK 的两个命令表）。
        let plan = build(temp.path(), &[node_tool(&dir), python_tool(&dir)], "C:\\a");
        assert_eq!(plan.prepend().len(), 1, "{:?}", plan.prepend());
        assert_eq!(plan.path_after(), format!("{};C:\\a", dir.display()));
    }
}
