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

use std::path::Path;

use crate::detect::DetectContext;

use super::super::files::{GlobalPackage, GlobalRow, GlobalsFile};
use super::{COMMAND_TIMEOUT, Command, find_on_path, first_line, has_version_segment};

/// 枚举全局包的超时（决策 170）。
///
/// **与 `probe_timeout` 分开**：后者是为 `--version` 定的几秒，而 `npm ls -g` 实测冷
/// **12.4 s**、热 0.77 s。用探测超时会把它判成"枚举失败"——那是**假阴性**，
/// 而假阴性在这里等于"新机器上少了几个全局包"。
pub(crate) const GLOBALS_TIMEOUT: std::time::Duration = COMMAND_TIMEOUT;

/// 判不出工具版本时写进 `tool_version` 的值（决策 172：**永远出键**，值可以是这个 slug）。
const UNKNOWN: &str = "unknown";

/// npm 那行的工具名（决策 171：工具表只有 `npm` 与 `pip`）。
const NPM: &str = "npm";
/// npm 的**命令名**：在进程 `%Path%` 上找的是它（决策 171：找不到就不产生行）。
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

/// pip 那行的工具名。
const PIP: &str = "pip";
/// pip 的命令名。
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

/// `enumerate_error` 的四个稳定 slug（决策 175）。
const COMMAND_FAILED: &str = "command-failed";
const TIMED_OUT: &str = "timed-out";
const BAD_JSON: &str = "bad-json";
const UNSUPPORTED_OUTPUT: &str = "unsupported-output";

/// 采集全局包清单。
///
/// 行的顺序固定：npm 在前、pip 在后（工具表的顺序是这一节的一部分，不按机器状态变）。
pub(crate) fn collect_globals(ctx: &DetectContext<'_>, captured_at: &str) -> GlobalsFile {
    let mut file = GlobalsFile::new(captured_at);
    if let Some(row) = npm_row(ctx) {
        file.global.push(row);
    }
    if let Some(row) = pip_row(ctx) {
        file.global.push(row);
    }
    file
}

/// npm 那一行。**可执行文件不在 PATH 上就没有行**（决策 171）。
fn npm_row(ctx: &DetectContext<'_>) -> Option<GlobalRow> {
    find_on_path(ctx, NPM_COMMAND)?;

    let tool_version = probed_version(ctx, &NODE_VERSION);
    let prefix = probed_prefix(ctx, &NPM_PREFIX);
    let (packages, enumerate_error) = enumerate(ctx, &NPM_PACKAGES, parse_npm_packages);

    Some(row(
        ctx,
        NPM,
        tool_version,
        prefix,
        packages,
        enumerate_error,
    ))
}

/// pip 那一行。**可执行文件不在 PATH 上就没有行**（决策 171）。
fn pip_row(ctx: &DetectContext<'_>) -> Option<GlobalRow> {
    find_on_path(ctx, PIP_COMMAND)?;

    // `--no-version` 关掉的就是这条命令（它按名字就是一个版本探测）。
    let answer = probed(ctx, &PIP_VERSION);
    let tool_version = answer
        .as_deref()
        .and_then(python_version)
        .unwrap_or_else(|| UNKNOWN.to_owned());
    let prefix = answer.as_deref().and_then(pip_prefix);
    let (packages, enumerate_error) = enumerate(ctx, &PIP_PACKAGES, parse_pip_packages);

    Some(row(
        ctx,
        PIP,
        tool_version,
        prefix,
        packages,
        enumerate_error,
    ))
}

/// 组装一行。`prefix` 与 `prefix_inside_version_dir` 在这里**同生共死**（决策 174）：
/// 只有一个 `Option<String>` 能造出这一对，所以"只出一个键"在类型上就写不出来。
fn row(
    ctx: &DetectContext<'_>,
    tool: &str,
    tool_version: String,
    prefix: Option<String>,
    packages: Vec<GlobalPackage>,
    enumerate_error: Option<String>,
) -> GlobalRow {
    GlobalRow {
        tool: tool.to_owned(),
        tool_version,
        prefix_inside_version_dir: prefix
            .as_deref()
            .map(|prefix| inside_version_dir(ctx, prefix)),
        prefix,
        packages,
        enumerate_error,
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
fn enumerate(
    ctx: &DetectContext<'_>,
    command: &Command,
    parse: fn(&str) -> Result<Vec<GlobalPackage>, &'static str>,
) -> (Vec<GlobalPackage>, Option<String>) {
    let outcome = command.run(ctx, GLOBALS_TIMEOUT);
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

    /// 同一个假机器跑两次，除 `captured_at` 外逐字节相同（确定性是幂等性的一部分）。
    #[test]
    fn two_runs_of_the_same_machine_are_identical() {
        let description = machine();
        assert_eq!(collect(&description), collect(&description));
    }
}
