//! 已知工具的**规格表**：怎么找、怎么读版本、怎么判断归属。
//!
//! **为什么是数据不是代码**：检测的六个来源（`PATH` 解析 / `App Paths` / ARP 卸载键 /
//! 文件系统扫描 / 版本管理器识别 / 版本探测）都需要同一份"工具是什么"的知识。
//! 把这份知识写成表，六个来源就只是对同一张表的不同查询 —— 而不是六段各自硬编码
//! 工具名的代码（那样加一个工具要改六处，漏一处就是静默漏检）。
//!
//! **版本探测的读法必须按工具分别配置**：本机实测 `git --version` 与 `java -version`
//! 输出到 **stderr**，而 `node -v` 到 **stdout**。只读一路会漏掉一半工具。

use serde::{Deserialize, Serialize};

/// 版本号从哪一路读。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VersionStream {
    /// 只看 stdout（`node -v`）。
    Stdout,
    /// 只看 stderr（`java -version` / `git --version`）。
    Stderr,
    /// 两路都看（不确定时的安全选择）。
    Both,
}

/// 一个工具要暴露的可执行文件名。
///
/// **同一个工具可能有多个名字**（`python` / `python3`；`node` 与 `npm` 属同一安装但
/// 是不同命令）。这里描述的是"能代表这个工具身份"的那些名字。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutableName {
    /// 文件名（含扩展名，`PATH` 解析就是按这个名字找的）。
    pub file: &'static str,
    /// 逻辑命令名（不含扩展名，用于展示与去重）。
    pub command: &'static str,
    /// **这个工具的主命令**。
    ///
    /// 一次 `detect` 的真实输出让这条变得必需：`node` 报了 4 行
    /// （`node.exe` / `npm.cmd` / `npx.cmd` / `corepack.cmd`），
    /// 但用户想知道的是"Node 是哪个版本、装在哪"，不是"npm 是哪个版本"。
    /// 主命令决定这个工具在报告里的**版本与路径**；非主命令仍然被发现、
    /// 仍然会被 shim（`npm` 确实要在 PATH 上），但不重复占用报告的行。
    pub primary: bool,
}

impl ExecutableName {
    /// 主命令。
    #[must_use]
    pub const fn primary(file: &'static str, command: &'static str) -> Self {
        Self {
            file,
            command,
            primary: true,
        }
    }

    /// 随行的次要命令（同一个工具带的其它可执行文件）。
    #[must_use]
    pub const fn secondary(file: &'static str, command: &'static str) -> Self {
        Self {
            file,
            command,
            primary: false,
        }
    }
}

/// 一个已知工具的规格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolSpec {
    /// 逻辑工具名（小写）。**这是 `DetectedTool::name` 的取值。**
    pub id: &'static str,
    /// 显示名。
    pub display_name: &'static str,
    /// 代表这个工具身份的可执行文件。
    pub executables: &'static [ExecutableName],
    /// 版本探测的参数（如 `["-v"]`）。
    pub version_args: &'static [&'static str],
    /// 版本从哪一路读。
    pub version_stream: VersionStream,
    /// 版本号前面要去掉的固定前缀（如 Java 的 `version `）。
    pub version_prefixes: &'static [&'static str],
}

impl ToolSpec {
    /// 版本输出里，把不需要的固定前缀削掉。
    ///
    /// 例：`java -version` 的第一行是 `openjdk version "1.8.0_492"` ——
    /// 我们要的是 `1.8.0_492`，而不是把整行当版本。
    #[must_use]
    pub fn strip_prefixes<'a>(&self, line: &'a str) -> &'a str {
        let mut text = line.trim();
        // 引号是 Java 输出的固定形状，先削。
        text = text.trim_matches('"');
        for prefix in self.version_prefixes {
            if let Some(rest) = text.strip_prefix(prefix) {
                text = rest.trim().trim_matches('"');
            }
        }
        text
    }
}

const NODE_EXES: &[ExecutableName] = &[
    ExecutableName::primary("node.exe", "node"),
    ExecutableName::secondary("npm.cmd", "npm"),
    ExecutableName::secondary("npx.cmd", "npx"),
    ExecutableName::secondary("corepack.cmd", "corepack"),
];

const PYTHON_EXES: &[ExecutableName] = &[
    ExecutableName::primary("python.exe", "python"),
    ExecutableName::secondary("python3.exe", "python3"),
    ExecutableName::secondary("pip.exe", "pip"),
    ExecutableName::secondary("pip3.exe", "pip3"),
];

const JAVA_EXES: &[ExecutableName] = &[
    ExecutableName::primary("java.exe", "java"),
    ExecutableName::secondary("javac.exe", "javac"),
    ExecutableName::secondary("jar.exe", "jar"),
    ExecutableName::secondary("jshell.exe", "jshell"),
];

const GIT_EXES: &[ExecutableName] = &[
    ExecutableName::primary("git.exe", "git"),
    ExecutableName::secondary("git-lfs.exe", "git-lfs"),
];

const UV_EXES: &[ExecutableName] = &[
    ExecutableName::primary("uv.exe", "uv"),
    ExecutableName::secondary("uvx.exe", "uvx"),
];

/// 已知工具表。**顺序即输出顺序**（`--json` 必须逐字节稳定，决策 35）。
///
/// 加一个工具只需要在这里加一条 —— 六个检测来源会自动覆盖它。
pub const KNOWN_TOOLS: &[ToolSpec] = &[
    ToolSpec {
        id: "node",
        display_name: "Node.js",
        executables: NODE_EXES,
        version_args: &["-v"],
        version_stream: VersionStream::Stdout,
        version_prefixes: &["v"],
    },
    ToolSpec {
        id: "python",
        display_name: "Python",
        executables: PYTHON_EXES,
        version_args: &["--version"],
        // CPython 把 `--version` 输出到 **stdout**，但 Windows 上的 App Execution Alias
        // 会走另一条路（直接报"未安装"），而某些发行版写 stderr —— 两路都读最稳。
        version_stream: VersionStream::Both,
        version_prefixes: &["Python "],
    },
    ToolSpec {
        id: "java",
        display_name: "Java 运行时",
        executables: JAVA_EXES,
        version_args: &["-version"],
        // **本机实测：`java -version` 走 stderr。**
        version_stream: VersionStream::Stderr,
        // `Pack200 ` 是本机 `jar -version` 的第一行前缀（`Pack200 1.8.0_492`）——
        // 不削它就会把 `Pack200` 报成版本。
        version_prefixes: &[
            "openjdk version ",
            "java version ",
            "openjdk ",
            "Pack200 ",
            "java ",
        ],
    },
    ToolSpec {
        id: "git",
        display_name: "Git",
        executables: GIT_EXES,
        // **本机实测：这个 Git 是 VFS for Git 构建，`git --version` 走 stdout**
        // （`git version 2.53.0.vfs.0.7`），而标准 Git for Windows 走 stderr。
        // 两路都读 —— 只读一路会让另一路的用户看到"发现但版本未知"。
        version_stream: VersionStream::Both,
        version_args: &["--version"],
        version_prefixes: &["git version "],
    },
    ToolSpec {
        id: "uv",
        display_name: "uv",
        executables: UV_EXES,
        version_args: &["--version"],
        version_stream: VersionStream::Both,
        version_prefixes: &["uv "],
    },
    ToolSpec {
        id: "cargo",
        display_name: "Rust 工具链（cargo）",
        executables: &[ExecutableName::primary("cargo.exe", "cargo")],
        version_args: &["--version"],
        version_stream: VersionStream::Stdout,
        version_prefixes: &["cargo "],
    },
    ToolSpec {
        id: "rustc",
        display_name: "Rust 编译器",
        executables: &[ExecutableName::primary("rustc.exe", "rustc")],
        version_args: &["--version"],
        version_stream: VersionStream::Stdout,
        version_prefixes: &["rustc "],
    },
    ToolSpec {
        id: "maven",
        display_name: "Apache Maven",
        executables: &[ExecutableName::primary("mvn.cmd", "mvn")],
        version_args: &["-v"],
        version_stream: VersionStream::Stdout,
        version_prefixes: &["Apache Maven "],
    },
    ToolSpec {
        id: "dotnet",
        display_name: ".NET",
        executables: &[ExecutableName::primary("dotnet.exe", "dotnet")],
        version_args: &["--version"],
        version_stream: VersionStream::Stdout,
        version_prefixes: &[],
    },
    ToolSpec {
        id: "go",
        display_name: "Go",
        executables: &[ExecutableName::primary("go.exe", "go")],
        version_args: &["version"],
        version_stream: VersionStream::Stdout,
        version_prefixes: &["go version "],
    },
    ToolSpec {
        id: "pnpm",
        display_name: "pnpm",
        executables: &[ExecutableName::primary("pnpm.cmd", "pnpm")],
        version_args: &["--version"],
        version_stream: VersionStream::Both,
        version_prefixes: &[],
    },
    ToolSpec {
        id: "wsl",
        display_name: "WSL",
        executables: &[ExecutableName::primary("wsl.exe", "wsl")],
        version_args: &["--version"],
        version_stream: VersionStream::Both,
        version_prefixes: &["WSL version: "],
    },
];

/// 按可执行文件名查规格（忽略大小写）。
#[must_use]
pub fn spec_for_file(file_name: &str) -> Option<&'static ToolSpec> {
    KNOWN_TOOLS.iter().find(|spec| {
        spec.executables
            .iter()
            .any(|exe| exe.file.eq_ignore_ascii_case(file_name))
    })
}

/// 按逻辑工具名查规格。
#[must_use]
pub fn spec_for_id(id: &str) -> Option<&'static ToolSpec> {
    KNOWN_TOOLS
        .iter()
        .find(|spec| spec.id.eq_ignore_ascii_case(id))
}

/// 显示名与注册表 `DisplayName` 之间的匹配规则。
///
/// **为什么不能直接按 id 匹配**：注册表里的名字是 `Node.js`、`Python 3.12.10 (64-bit)`、
/// `Microsoft Visual Studio Code` 这类**自由文本**，而且不同机制写法不同
/// （winget 的 ARP 名字、官方安装器的 ARP 名字、`py -0p` 的路径）。
/// 所以这里用"显示名 + 显式别名"做大小写不敏感的子串匹配。
///
/// **短名字必须走词边界匹配**。这条不是洁癖：`Go` 作为子串出现在 `Django` 里，
/// `Git` 出现在 `Digital` 里，而 `Django Web Framework` 是真实会出现在 ARP 里的
/// 显示名形状。若用 `contains`，一台装着 Django 的机器会被报成"装了 Go" ——
/// 这正是"宁可不报，也不要报错"的反面。
///
/// **必须包含的别名**（来自本机取证）：
/// - `openjdk` / `temurin` / `oracle` → Java
/// - `python` 的 ARP 名会带版本与 `(64-bit)`
/// - `git for windows` / `git-lfs` 是 Git 的真实显示名
#[must_use]
pub fn spec_for_display_name(display_name: &str) -> Option<&'static ToolSpec> {
    let needle = display_name.to_lowercase();
    for spec in KNOWN_TOOLS {
        let name = spec.display_name.to_lowercase();
        // 3 个字符以内（`Go`）走词边界，更长的不可能藏在别的词里（`Node.js`）。
        if if name.chars().count() <= 3 {
            id_appears_as_a_word(&needle, &name)
        } else {
            needle.contains(&name)
        } {
            return Some(spec);
        }
        if id_appears_as_a_word(&needle, spec.id) {
            return Some(spec);
        }
        for alias in aliases_for(spec.id) {
            if needle.contains(alias) {
                return Some(spec);
            }
        }
    }
    None
}

/// 显式别名表。**加别名要有取证依据**，不要凭印象加。
fn aliases_for(id: &str) -> &'static [&'static str] {
    match id {
        "java" => &[
            "openjdk",
            "temurin",
            "oracle jdk",
            "jdk",
            "jre",
            "eclipse adoptium",
        ],
        "python" => &["python launcher"],
        "node" => &["nodejs"],
        "git" => &["git for windows", "git-lfs"],
        "dotnet" => &[".net", "dotnet"],
        "wsl" => &["windows subsystem for linux"],
        "maven" => &["apache maven"],
        "rustc" | "cargo" => &["rust"],
        _ => &[],
    }
}

/// `id` 是否作为一个**独立的词**出现在文本里。
///
/// 防止 `go` 命中 `Django`、`git` 命中 `digital`。
fn id_appears_as_a_word(haystack: &str, id: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(id) {
        let at = start + pos;
        let before_ok = at == 0
            || !haystack[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after = at + id.len();
        let after_ok = after >= haystack.len()
            || !haystack[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = at + 1;
    }
    false
}

/// 从一条路径里，取出**看起来像版本**的那一段。
///
/// 用于"只有目录、没有注册表记录"的情况（本机 `C:\Dev\Tool\apache-maven-3.9.5`）。
/// 取不到就返回 `None` —— 上层记"发现但版本未知"。
///
/// **两级形状都要认**，而且这是从本机取证里学到的：
/// - 版本**自己就是一段**：`...\nvm\v24.19.0` → `24.19.0`
/// - 版本**藏在名字尾巴里**：`apache-maven-3.9.5` → `3.9.5`、`JDK17` → `17`
///
/// 第二级不能简单取"最后一个 `-` 之后"：`temurin21-binaries` 的尾段是
/// `binaries`，没有版本。所以规则是**从右往左找第一段以数字开头的分隔片段**。
#[must_use]
pub fn version_from_path_hint(path: &str) -> Option<String> {
    // 从后往前找第一段含数字的路径组件。
    let segment = path.split(['\\', '/']).rev().find(|segment| {
        looks_like_version(segment) && segment.chars().any(|c| c.is_ascii_digit())
    })?;

    // 第一级：整段就是一个版本（`v24.19.0` / `21.0.12.1+1`）。`v` 前缀先削掉。
    let whole = trim_version_edges(segment.trim_start_matches(['v', 'V']));
    if looks_like_version(whole) && whole.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Some(whole.to_owned());
    }

    // 第二级：**以最后一个 `-` / `_` 为界，取右边那一段**。
    //
    // 这条规则同时覆盖两种真实形状：
    // - `apache-maven-3.9.5` → 最后一个 `-` 之后是 `3.9.5`
    // - `node-v24.19.0` → 最后一个 `-` 之后是 `v24.19.0`（`v` 再削一次）
    // - `jdk-21.0.12.1+1` → `21.0.12.1+1`
    //
    // **为什么不用"向左收集版本字符"**：`-` 是版本字符，所以那种做法会把
    // `maven-3.9.5` 整段收进来。分隔符才是这里唯一可靠的锚。
    let tail = segment.rsplit(['-', '_']).next().unwrap_or(segment);
    let tail = trim_version_edges(tail.trim_start_matches(['v', 'V']));
    if looks_like_version(tail) && tail.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Some(tail.to_owned());
    }

    // 第三级：没有分隔符时（`JDK17`），**剥掉前导字母**。
    let last_digit = segment.rfind(|c: char| c.is_ascii_digit())?;
    let start = segment[..=last_digit]
        .rfind(|c: char| !c.is_ascii_digit())
        .map_or(0, |i| i + 1);
    let token = trim_version_edges(&segment[start..]);
    if looks_like_version(token) && token.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Some(token.to_owned());
    }
    None
}

/// 削掉版本串两端的非版本字符（引号、斜杠、空白、尾随点）。
///
/// **削掉的是两端，不是左边的字母** —— `JDK17` 的左字母在第二级拆分时已经由
/// `-`/`_` 之外的处理交代过了，这里只处理"两端"。
fn trim_version_edges(text: &str) -> &str {
    text.trim_matches(|c: char| {
        !c.is_ascii_alphanumeric() && c != '.' && c != '_' && c != '+' && c != '-'
    })
}

/// 全部需要探测的 `PATH` 文件名（小写，用于大小写不敏感匹配）。
#[must_use]
pub fn all_executable_files() -> Vec<&'static str> {
    KNOWN_TOOLS
        .iter()
        .flat_map(|spec| spec.executables.iter().map(|exe| exe.file))
        .collect()
}

/// 从一个工具的输出里抽出版本号。
///
/// **不用正则**（不引入依赖，也不需要回溯）：
/// 逐行剥掉已知前缀，然后取第一个"看起来像版本"的 token。
/// `1.8.0_492`、`v24.19.0`、`21.0.12.1+1`、`3.9.5`、`0.12.22` 都能取到；
/// 取不到时返回 `None` —— 上层会记录"发现但版本未知"，**不编一个版本出来**。
#[must_use]
pub fn extract_version(spec: &ToolSpec, text: &str) -> Option<String> {
    let text = normalize_tool_output(text);
    for line in text.lines() {
        // **以 `[` 开头的行是弃用/警告横幅，不是版本。** 本机实测：
        // `jar -version` 的第一行是 `[0.003s][warning][cds] Archived non-system classes…`，
        // 里面含数字 —— 不排除它会把 `[0.003s][warning][cds]` 或一个路径当版本号。
        if line.trim_start().starts_with('[') {
            continue;
        }
        let stripped = spec.strip_prefixes(line);
        if let Some(token) = first_version_token(stripped) {
            return Some(token);
        }
    }
    None
}

/// 把工具输出里夹杂的 **UTF-16LE** 还原成 UTF-8。
///
/// **这不是洁癖，是真机事实**：本机 `wsl --version` 的输出是 UTF-16LE ——
/// 按 UTF-8 读出来是 `W\0S\0L\0 \0`，每个 token 都被 NUL 隔断，
/// 于是 `extract_version` 一个 token 都取不到，用户看到"发现但版本未知"。
///
/// 判定方法：**奇数位大量是 `0x00`**。ASCII 被 UTF-16LE 编码后形如
/// `W\0S\0L\0`（低位在前、高位是 NUL），所以 NUL 集中在**奇数**下标。
/// 真实的 UTF-8 文本（哪怕含中文）不会长成这样。
///
/// 判定不通过就原样返回，**不做有损转换** —— 猜错方向的代价是把好好的输出弄坏。
fn normalize_tool_output(text: &str) -> String {
    let bytes = text.as_bytes();
    if bytes.len() < 8 {
        return text.to_owned();
    }
    // **奇数长度不能直接放弃。** 真机踩到的坑：`ProcessOutcome::combined()` 会把
    // stdout 与 stderr 拼起来并在中间插一个 `\n`，所以 stdout 是 UTF-16、
    // stderr 为空时，总长度是奇数 —— 直接 `return` 会让 `wsl --version` 的
    // 版本号永远抠不出来。削掉尾部那个孤立的字节再判。
    let pairs = bytes.len() / 2;
    let usable = &bytes[..pairs * 2];

    let odd_nul = usable
        .iter()
        .skip(1)
        .step_by(2)
        .filter(|b| **b == 0)
        .count();
    let even_nul = usable.iter().step_by(2).filter(|b| **b == 0).count();
    // 奇数位至少四成是 NUL，且明显多于偶数位（防住"恰好含很多 NUL 的二进制"）。
    if odd_nul * 5 < pairs * 2 || odd_nul < even_nul * 4 {
        return text.to_owned();
    }
    let units: Vec<u16> = usable
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
}

/// 取第一个含数字且只由版本字符组成的 token。
fn first_version_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|token| token.trim_matches(|c: char| c == '"' || c == '\'' || c == '(' || c == ')'))
        .find(|token| looks_like_version(token))
        .map(ToOwned::to_owned)
}

/// 版本字符集：数字、点、下划线、加号、连字符，且**至少含一个数字**。
///
/// 连字符与加号是必需的：`21.0.12.1+1`（Temurin）、`0.1.0-rc.6`（npm 包版本）
/// 都是真实存在的形状。
fn looks_like_version(token: &str) -> bool {
    if token.is_empty() || !token.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    token
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_looked_up_by_file_name_case_insensitively() {
        assert_eq!(spec_for_file("NODE.EXE").map(|s| s.id), Some("node"));
        assert_eq!(spec_for_file("node.exe").map(|s| s.id), Some("node"));
        assert_eq!(spec_for_file("javac.exe").map(|s| s.id), Some("java"));
        assert_eq!(spec_for_file("nope.exe"), None);
    }

    #[test]
    fn every_spec_has_at_least_one_executable_and_a_unique_id() {
        let mut seen = std::collections::HashSet::new();
        for spec in KNOWN_TOOLS {
            assert!(!spec.executables.is_empty(), "{} 没有可执行文件", spec.id);
            assert!(
                seen.insert(spec.id),
                "工具 id `{}` 重复 —— 去重会出错",
                spec.id
            );
            assert!(
                spec.id.chars().all(|c| c.is_ascii_lowercase()),
                "{} 的 id 必须小写",
                spec.id
            );
        }
    }

    #[test]
    fn no_executable_file_is_claimed_by_two_tools() {
        // 两个工具抢同一个文件名会让 PATH 解析的结果取决于表的顺序 —— 那是隐式行为。
        let mut seen = std::collections::HashSet::new();
        for spec in KNOWN_TOOLS {
            for exe in spec.executables {
                assert!(
                    seen.insert(exe.file.to_lowercase()),
                    "{} 被两个工具声明",
                    exe.file
                );
            }
        }
    }

    #[test]
    fn node_version_comes_from_stdout_with_a_v_prefix() {
        let spec = spec_for_id("node").expect("node");
        assert_eq!(spec.version_stream, VersionStream::Stdout);
        assert_eq!(
            extract_version(spec, "v24.19.0\n").as_deref(),
            Some("24.19.0")
        );
    }

    #[test]
    fn java_version_comes_from_stderr_and_drops_the_quotes() {
        let spec = spec_for_id("java").expect("java");
        assert_eq!(spec.version_stream, VersionStream::Stderr);
        let stderr = "openjdk version \"1.8.0_492\"\nOpenJDK Runtime Environment\n";
        assert_eq!(extract_version(spec, stderr).as_deref(), Some("1.8.0_492"));
        // 另一种厂商的输出形状。
        assert_eq!(
            extract_version(spec, "java version \"1.8.0_491\"\n").as_deref(),
            Some("1.8.0_491")
        );
    }

    #[test]
    fn git_version_is_read_from_either_stream() {
        // **本机实测两路都有**：标准 Git for Windows 走 stderr，
        // 而这个 Git 是 VFS for Git 构建、走 stdout（`git version 2.53.0.vfs.0.7`）。
        // 只读一路会让另一路的用户看到"发现但版本未知"。
        let spec = spec_for_id("git").expect("git");
        assert_eq!(spec.version_stream, VersionStream::Both);
        assert_eq!(
            extract_version(spec, "git version 2.51.0.windows.1\n").as_deref(),
            Some("2.51.0.windows.1")
        );
        assert_eq!(
            extract_version(spec, "git version 2.53.0.vfs.0.7\n").as_deref(),
            Some("2.53.0.vfs.0.7")
        );
    }

    #[test]
    fn temurin_style_plus_version_survives_extraction() {
        let spec = spec_for_id("java").expect("java");
        assert_eq!(
            extract_version(spec, "openjdk version \"21.0.12.1+1\"\n").as_deref(),
            Some("21.0.12.1+1")
        );
    }

    #[test]
    fn cargo_style_prerelease_version_survives_extraction() {
        let spec = spec_for_id("cargo").expect("cargo");
        assert_eq!(
            extract_version(spec, "cargo 1.99.0 (5f94df478 2026-08-27)\n").as_deref(),
            Some("1.99.0")
        );
        let spec = spec_for_id("pnpm").expect("pnpm");
        assert_eq!(
            extract_version(spec, "0.1.0-rc.6\n").as_deref(),
            Some("0.1.0-rc.6")
        );
    }

    #[test]
    fn go_version_line_is_stripped_before_extraction() {
        let spec = spec_for_id("go").expect("go");
        assert_eq!(
            extract_version(spec, "go version go1.24.3 windows/amd64\n").as_deref(),
            Some("go1.24.3")
        );
    }

    #[test]
    fn unparseable_output_yields_none_instead_of_an_invented_version() {
        let spec = spec_for_id("node").expect("node");
        // 工具报错、或输出是本地化的提示 —— 绝不编一个版本出来。
        assert_eq!(extract_version(spec, ""), None);
        assert_eq!(extract_version(spec, "command not found\n"), None);
        assert_eq!(extract_version(spec, "未安装\n"), None);
    }

    #[test]
    fn utf16le_output_is_decoded_before_extraction() {
        // **本机实测**：`wsl --version` 输出 UTF-16LE。按 UTF-8 读是
        // `W\0S\0L\0 \0H\0r\0...`，每个 token 都被 NUL 隔断 —— 一个都取不到。
        let spec = spec_for_id("wsl").expect("wsl");
        let utf16: Vec<u8> = "WSL 版本： 2.7.11.0\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let lossy = String::from_utf8_lossy(&utf16).into_owned();
        assert_eq!(
            extract_version(spec, &lossy).as_deref(),
            Some("2.7.11.0"),
            "UTF-16LE 必须先被还原"
        );
    }

    #[test]
    fn utf16le_survives_the_odd_length_that_combined_output_produces() {
        // **这条是真机踩出来的**：`ProcessOutcome::combined()` 在 stdout 与 stderr
        // 之间插一个 `\n`，所以 stdout 是 UTF-16、stderr 为空时总长度是**奇数**。
        // 早先的实现对奇数长度直接放弃，于是 `wsl --version` 的版本号永远抠不出来
        // —— 而且它看起来像"wsl 没有版本"，不像 bug。
        let spec = spec_for_id("wsl").expect("wsl");
        let utf16: Vec<u8> = "WSL 版本： 2.7.11.0\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let lossy = String::from_utf8_lossy(&utf16).into_owned();
        let with_empty_stderr = format!("{lossy}\n");
        assert!(
            !with_empty_stderr.len().is_multiple_of(2),
            "这个用例的前提是拼接后长度为奇数"
        );
        assert_eq!(
            extract_version(spec, &with_empty_stderr).as_deref(),
            Some("2.7.11.0")
        );
    }

    #[test]
    fn ordinary_utf8_output_is_not_mistaken_for_utf16() {
        // 猜错方向的代价是把好好的输出弄坏，所以这条必须钉住。
        let spec = spec_for_id("node").expect("node");
        assert_eq!(
            extract_version(spec, "v24.19.0\n").as_deref(),
            Some("24.19.0")
        );
        // 含中文的 UTF-8 输出同样不能被当成 UTF-16。
        let spec = spec_for_id("python").expect("python");
        assert_eq!(
            extract_version(spec, "Python 3.12.10 —— 这是中文说明\n").as_deref(),
            Some("3.12.10")
        );
    }

    #[test]
    fn a_warning_banner_line_is_never_the_version() {
        // **本机实测**：`jar -version` 的第一行是
        // `[0.003s][warning][cds] Archived non-system classes…`，含数字。
        let spec = spec_for_id("java").expect("java");
        let stderr =
            "[0.003s][warning][cds] Archived non-system classes are disabled\nPack200 1.8.0_492\n";
        assert_eq!(
            extract_version(spec, stderr).as_deref(),
            Some("1.8.0_492"),
            "警告横幅不得被当成版本"
        );
    }

    #[test]
    fn a_bare_number_is_not_mistaken_for_a_version_of_the_wrong_tool() {
        // `python --version` 在某些别名上会输出一句中文提示，其中可能含数字。
        // 我们只接受"整 token 都是版本字符"的形状，所以纯中文句子不会被误读。
        let spec = spec_for_id("python").expect("python");
        assert_eq!(
            extract_version(spec, "Python 3.12.10\n").as_deref(),
            Some("3.12.10")
        );
        assert_eq!(
            extract_version(spec, "Python was not found; run without arguments\n"),
            None
        );
    }

    #[test]
    fn display_name_matching_handles_the_real_registry_names() {
        // 本机 ARP 里真实存在的形状。
        assert_eq!(spec_for_display_name("Node.js").map(|s| s.id), Some("node"));
        assert_eq!(
            spec_for_display_name("Python 3.12.10 (64-bit)").map(|s| s.id),
            Some("python")
        );
        assert_eq!(
            spec_for_display_name("Eclipse Temurin JDK with Hotspot 21.0.12.1+1 (x64)")
                .map(|s| s.id),
            Some("java")
        );
        assert_eq!(
            spec_for_display_name("Java 8 Update 491").map(|s| s.id),
            Some("java")
        );
        assert_eq!(spec_for_display_name("Git").map(|s| s.id), Some("git"));
        assert_eq!(
            spec_for_display_name("Microsoft Visual Studio Code").map(|s| s.id),
            None,
            "VS Code 不是我们的已知工具，不该被硬塞进某一条"
        );
    }

    #[test]
    fn short_ids_do_not_match_inside_other_words() {
        // `go` 不能命中 `Django`，`git` 不能命中 `digital`。
        assert_eq!(
            spec_for_display_name("Django Web Framework").map(|s| s.id),
            None
        );
        assert_eq!(
            spec_for_display_name("Digital Signature Tool").map(|s| s.id),
            None
        );
        // 但独立成词时必须命中。
        assert_eq!(
            spec_for_display_name("Go Programming Language").map(|s| s.id),
            Some("go")
        );
    }

    #[test]
    fn version_hint_reads_the_trailing_version_directory() {
        assert_eq!(
            version_from_path_hint(r"C:\Dev\Tool\apache-maven-3.9.5").as_deref(),
            Some("3.9.5")
        );
        assert_eq!(
            version_from_path_hint(r"C:\Dev\base\JDK\JDK17").as_deref(),
            Some("17")
        );
        assert_eq!(version_from_path_hint(r"C:\Tools\mytool").as_deref(), None);
        assert_eq!(
            version_from_path_hint(r"C:\Program Files\Common Files").as_deref(),
            None
        );
    }
}
