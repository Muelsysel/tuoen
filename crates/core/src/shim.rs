//! 一个工具**要在 `PATH` 上暴露哪些命令**的目录。
//!
//! # 为什么这份知识要单独有一张表
//!
//! 检测（[`crate::detect`]）回答的是"这台机器上装了什么"，这张表回答的是
//! "装好之后要往 `PATH` 上放什么"。两者都建立在"一个工具是什么"这件事上，
//! 但**要的东西不一样**：检测要的是可执行文件名与版本读法（`detect::spec::KNOWN_TOOLS`），
//! 这里要的是「真 `.exe` + 前缀参数」的等价写法。
//!
//! 写成表而不是代码，理由与 `KNOWN_TOOLS` 一样：加一个工具只改一处。
//!
//! # 为什么有的命令需要前缀参数
//!
//! 因为 `npm` / `npx` / `corepack` 在 Windows 上**只有 `.cmd` 启动器**，
//! 而 shim 只能转发到**可执行映像**：`.cmd` 会被 `tuoen_shim::write_shim` 拒成
//! `script-target`。理由写在 `crates/shim/src/lib.rs` 的 crate 文档里，一句话版本是 ——
//! `CreateProcessW` 碰到 `.cmd` 会走 `%COMSPEC% /c`，而 cmd 的 `/c` 会剥掉整行的
//! 首尾引号（目标根本跑不起来），并且仍会展开参数里的 `%VAR%`、把不加引号的 `&`
//! 当命令分隔符执行（shim 变成一条参数注入通道）。
//!
//! 正确的等价写法是「`node.exe` + 指向 `cli.js` 的前缀参数」。本机实测：
//! `node <npm-cli.js>` 与 `npm.cmd` 的输出**逐字节一致**。
//!
//! # 这张表里的每一条路径都必须实测过
//!
//! 猜错一条相对路径，症状是"shim 生成成功，敲命令的时候才发现目标不存在" ——
//! 失败被推迟到了最不该出现它的地方（我们明明可以在生成时就检查目标是否存在）。
//! 所以 [`shim_commands`] 对**没实测过**的工具返回空表：宁可不发 shim，也不猜。

/// 一个工具要在 `PATH` 上暴露的一条命令。
///
/// **这里的路径全都是相对的**（相对载荷根 `<store>/<tool>/current`）：
/// 生成 shim 时由调用方拼成绝对路径。相对路径是"切版本不必重新生成 shim"的
/// 前提之一（另一条是 shim 指向稳定的 `current`，见 `docs/DESIGN.md` 决策 11）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimCommand {
    /// 命令名（不带扩展名）。落盘是 `<command>.exe`。
    pub command: String,
    /// 载荷根下的相对路径，例如 `node.exe` / `node_modules/npm/bin/npm-cli.js`。
    ///
    /// 生成 shim 时由调用方拼到 `<store>/<tool>/current/` 后面。
    /// **永远用 `/` 分隔**：拼路径是平台的事（Windows 上 `Path::push` 会给出反斜杠），
    /// 而这里写死的是"归档里的形状"。
    pub relative: String,
    /// 前缀参数（相对路径，同样拼到 `current/` 后面）；通常是空。
    ///
    /// 为什么用相对路径而不是绝对路径：绝对路径只有在有人把它拼出来的那一刻才存在，
    /// 而这一层（`tuoen-core`）连 store 在哪都不知道 —— 它不该知道。
    pub prefix_args: Vec<String>,
    /// 为什么需要前缀参数 —— 一句话，进人类可读输出。
    pub note: Option<String>,
}

/// 一条命令的静态规格（编译期常量，`KNOWN_TOOLS` 的风格）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandSpec {
    command: &'static str,
    relative: &'static str,
    prefix_args: &'static [&'static str],
    note: Option<&'static str>,
}

/// 一个工具的静态规格。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ToolCommands {
    id: &'static str,
    commands: &'static [CommandSpec],
}

const NPM_NOTE: &str = "Windows 上 `npm` 只有 `npm.cmd` 启动器，而 `.cmd` 会被 shim 拒绝\
                        （`CreateProcessW` 走 `%COMSPEC% /c`，cmd 会剥掉整行的首尾引号，\
                        于是目标脚本根本跑不起来）—— 所以走 `node.exe` + `npm-cli.js` 这条路。\
                        实测 `node <npm-cli.js>` 与 `npm.cmd` 的输出逐字节一致。";

const NPX_NOTE: &str = "与 `npm` 同理：Windows 上只有 `npx.cmd`，而 `.cmd` 会被 shim 拒绝，\
                        所以走 `node.exe` + `npx-cli.js`。npx 与 npm 是同一次安装的两个入口，\
                        两者的 cli.js 都在 `node_modules/npm/bin/` 下。";

const COREPACK_NOTE: &str = "与 `npm` 同理：Windows 上只有 `corepack.cmd`，而 `.cmd` 会被 shim 拒绝，\
                             所以走 `node.exe` + `corepack.js`。注意 corepack 是独立目录\
                             （`node_modules/corepack/dist/corepack.js`），而且它**不是每个 Node 都有**\
                             （Node 14 及更早没有）—— 那时这条命令会以 `target-unreadable` 失败，
                             而其余几条照常生成。";

const NODE_COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        command: "node",
        relative: "node.exe",
        prefix_args: &[],
        note: None,
    },
    CommandSpec {
        command: "npm",
        relative: "node.exe",
        prefix_args: &["node_modules/npm/bin/npm-cli.js"],
        note: Some(NPM_NOTE),
    },
    CommandSpec {
        command: "npx",
        relative: "node.exe",
        prefix_args: &["node_modules/npm/bin/npx-cli.js"],
        note: Some(NPX_NOTE),
    },
    CommandSpec {
        command: "corepack",
        relative: "node.exe",
        prefix_args: &["node_modules/corepack/dist/corepack.js"],
        note: Some(COREPACK_NOTE),
    },
];

/// 某个命令名属于哪个工具。不认识返回 `None`。
///
/// 存在的理由是 `tuoen shim add` 与 `tuoen shim remove` 的**粒度不一样**：
/// `add` 收的是工具（`add node` 一次生成 4 条），`remove` 收的是命令名
/// （`remove node` 只删 `node.exe`）。这个不对称本身是合理的（`remove npm`
/// 必须能只删 npm），但它会让人以为 `remove node` 是 `add node` 的逆操作。
/// 有了这张反查表，`remove` 就能在删完之后**指名道姓地说出还剩哪几条**。
#[must_use]
pub fn tool_for_command(command: &str) -> Option<&'static str> {
    SHIM_COMMANDS
        .iter()
        .find(|tool| {
            tool.commands
                .iter()
                .any(|spec| spec.command.eq_ignore_ascii_case(command))
        })
        .map(|tool| tool.id)
}

/// 某个工具要在 `PATH` 上暴露哪些命令名（顺序即表的顺序）。不认识返回空表。
#[must_use]
pub fn command_names(tool: &str) -> Vec<&'static str> {
    SHIM_COMMANDS
        .iter()
        .find(|spec| spec.id.eq_ignore_ascii_case(tool))
        .map(|spec| spec.commands.iter().map(|c| c.command).collect())
        .unwrap_or_default()
}

/// 已知的「工具 → 要暴露的命令」表。**顺序即输出顺序**（`--json` 必须逐字节稳定，决策 35）。
///
/// 现在只有 `node` 一家。加一条之前先**在本机实测**：造一个真的安装目录，
/// 逐条跑一遍，确认相对路径存在、并且输出与官方启动器一致。
const SHIM_COMMANDS: &[ToolCommands] = &[ToolCommands {
    id: "node",
    commands: NODE_COMMANDS,
}];

/// 某个工具要在 `PATH` 上暴露哪些命令。**不认识、或还没实测过的工具返回空表。**
///
/// 目前只有 `node` 一家是逐条实测过的：`node` / `npm` / `npx` / `corepack`。
///
/// **`python` / `java` / `git` / `uv` / `cargo` 还没实测，所以是空的** ——
/// 它们的发行版各有自己的目录形状（`python.exe` 在根目录、`java.exe` 在 `bin/`、
/// `git.exe` 在 `cmd/` 或 `bin/` 要看是哪一种发行版），而"哪一个是确定存在的启动器"
/// 必须一条条验，不能凭印象写进表里。
///
/// **空表的意思是"我们不知道"，不是"这个工具没有命令"**：调用方
/// （`tuoen shim add`）必须把这两种情况分开报，否则用户会以为工具本身有问题。
///
/// 工具 id **大小写不敏感**（`Node` 与 `node` 是同一个工具）。
#[must_use]
pub fn shim_commands(tool: &str) -> Vec<ShimCommand> {
    let Some(spec) = SHIM_COMMANDS
        .iter()
        .find(|spec| spec.id.eq_ignore_ascii_case(tool))
    else {
        return Vec::new();
    };
    spec.commands
        .iter()
        .map(|command| ShimCommand {
            command: command.command.to_owned(),
            relative: command.relative.to_owned(),
            prefix_args: command
                .prefix_args
                .iter()
                .map(|arg| (*arg).to_owned())
                .collect(),
            note: command.note.map(str::to_owned),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成 shim 时会**拒绝**的扩展名。
    ///
    /// 这张表对应的是 `tuoen_shim::ShimError::ScriptTarget`那几种。
    /// 放在这里是为了让"将来有人手滑把 `.cmd` 加回表里"这件事在 `cargo test` 里就炸。
    const SCRIPT_EXTENSIONS: [&str; 3] = [".cmd", ".bat", ".ps1"];

    fn ends_with_script_extension(path: &str) -> bool {
        let lowered = path.to_ascii_lowercase();
        SCRIPT_EXTENSIONS
            .iter()
            .any(|extension| lowered.ends_with(extension))
    }

    #[test]
    fn node_exposes_the_four_measured_commands_in_a_stable_order() {
        let commands = shim_commands("node");
        let names: Vec<&str> = commands.iter().map(|c| c.command.as_str()).collect();
        assert_eq!(
            names,
            vec!["node", "npm", "npx", "corepack"],
            "顺序就是输出顺序 —— `--json` 必须逐字节稳定（决策 35）"
        );
    }

    #[test]
    fn every_command_has_a_non_empty_relative_path_that_is_not_a_script() {
        // **核心断言**：每一条命令的相对路径都非空，而且**没有一条指向脚本**。
        //
        // 后半条是防回归的：`.cmd` 目标会被 `write_shim` 拒成 `script-target`，
        // 于是 `tuoen shim add node` 会在"生成"这一步就失败 ——
        // 而如果哪天有人为了"支持 npm.cmd"把表改回去，他会在测试里立刻看到这条。
        for command in shim_commands("node") {
            assert!(
                !command.relative.is_empty(),
                "`{}` 的相对路径是空的 —— 拼出来的目标会落在 `current/` 本身（一个目录）",
                command.command
            );
            assert!(
                !ends_with_script_extension(&command.relative),
                "`{}` 的目标是脚本（{}）—— shim 拒绝脚本目标，见 crate 文档",
                command.command,
                command.relative
            );
            for arg in &command.prefix_args {
                assert!(!arg.is_empty(), "`{}` 有一个空前缀参数", command.command);
                assert!(
                    !ends_with_script_extension(arg),
                    "`{}` 的前缀参数是脚本（{arg}）—— 前缀参数必须是数据（比如 cli.js），\
                     不是另一个启动器",
                    command.command
                );
            }
        }
    }

    #[test]
    fn the_commands_that_need_a_prefix_go_through_a_js_entry_point() {
        // npm / npx / corepack 的判据很具体：目标是 `node.exe`，
        // 而前缀参数是一个 `.js` 文件。**"没有走 .cmd"的证据就在这两个事实里。**
        for (name, expected_relative, expected_arg) in [
            ("npm", "node.exe", "node_modules/npm/bin/npm-cli.js"),
            ("npx", "node.exe", "node_modules/npm/bin/npx-cli.js"),
            (
                "corepack",
                "node.exe",
                "node_modules/corepack/dist/corepack.js",
            ),
        ] {
            let command = shim_commands("node")
                .into_iter()
                .find(|c| c.command == name)
                .unwrap_or_else(|| panic!("`{name}` 应当在表里"));
            assert_eq!(command.relative, expected_relative, "{name} 的目标");
            assert_eq!(
                command.prefix_args,
                vec![expected_arg.to_owned()],
                "{name} 的前缀参数必须是指向 cli.js 的那一条"
            );
            assert!(
                command.prefix_args[0].ends_with(".js"),
                "{name} 的前缀参数必须是一个 `.js`"
            );
            assert!(command.note.is_some(), "{name} 需要解释为什么走这条路");
        }
    }

    #[test]
    fn node_itself_needs_no_prefix() {
        // `node` 是唯一一条不需要前缀参数的：它自己就是那个可执行映像。
        // 给它加前缀会让 `node --version` 变成 `node <某文件> --version`。
        let node = shim_commands("node")
            .into_iter()
            .find(|c| c.command == "node")
            .expect("node 应当在表里");
        assert_eq!(node.relative, "node.exe");
        assert!(node.prefix_args.is_empty());
        assert!(node.note.is_none(), "不需要前缀就不需要解释");
    }

    #[test]
    fn the_notes_name_the_mechanism_not_just_the_rule() {
        // 只说"不支持 .cmd"会让人以为是我们偷懒。说明必须包含三件事：
        // ①Windows 上只有 `.cmd`；②`.cmd` 会被拒绝；③我们改走了哪条路并且实测过。
        let npm = shim_commands("node")
            .into_iter()
            .find(|c| c.command == "npm")
            .expect("npm 应当在表里");
        let note = npm.note.expect("npm 必须有解释");
        assert!(note.contains(".cmd"), "要指出 Windows 上只有 .cmd：{note}");
        assert!(note.contains("shim"), "要指出 .cmd 会被 shim 拒绝：{note}");
        assert!(note.contains("cli.js"), "要指出改走了哪条路：{note}");
        assert!(
            note.contains("逐字节"),
            "要说清这条等价写法是实测的：{note}"
        );
    }

    #[test]
    fn tools_we_have_not_measured_get_an_empty_table() {
        // **刻意是空的**：这几种工具各自的发行版目录形状不同，而"哪一个是确定存在的
        // 启动器"必须实测。空表会让 `tuoen shim add python` 报一句"还不知道"，
        // 这比生成一个指向不存在文件的 shim 好得多。
        for tool in ["python", "java", "git", "uv", "cargo"] {
            assert!(
                shim_commands(tool).is_empty(),
                "`{tool}` 还没实测过启动器，不该凭空给出一条命令"
            );
        }
    }

    #[test]
    fn unknown_tools_get_an_empty_table() {
        for tool in ["definitely-not-a-tool", "", "nodejs", " node"] {
            assert!(shim_commands(tool).is_empty(), "`{tool}` 不该有命令");
        }
    }

    #[test]
    fn a_command_name_maps_back_to_its_tool() {
        // 这张反查表存在的唯一理由是 `shim remove` 收的是**命令名**而 `add` 收的是
        // **工具**，于是 `remove node` 只删一条 —— 反查让 remove 能说清还剩哪几条。
        for (command, tool) in [
            ("node", "node"),
            ("npm", "node"),
            ("npx", "node"),
            ("corepack", "node"),
        ] {
            assert_eq!(tool_for_command(command), Some(tool), "`{command}` 的归属");
        }
        // 大小写不敏感，与 `shim_commands` 一致。
        assert_eq!(tool_for_command("NPM"), Some("node"));
        // **不认识就说不认识**，不许猜一个工具出来 —— 猜错会让 `remove`
        // 报出几条根本不属于它的"漏下的兄弟"。
        for unknown in ["python", "pip", "", "nodejs", "node.exe"] {
            assert_eq!(tool_for_command(unknown), None, "`{unknown}` 不该有归属");
        }
    }

    #[test]
    fn command_names_agree_with_shim_commands() {
        // 两条 API 说的是同一件事，只是形状不同（一个是名字、一个是完整规格）。
        // 漂移的症状是 `remove` 提示的名字与 `add` 生成的文件对不上。
        let names = command_names("node");
        let commands = shim_commands("node");
        let from_commands: Vec<&str> = commands.iter().map(|c| c.command.as_str()).collect();
        assert_eq!(names, from_commands);
        assert_eq!(names, vec!["node", "npm", "npx", "corepack"]);
        // 大小写不敏感，不认识返回空表（**不是**报错）。
        assert_eq!(command_names("NoDe").len(), 4);
        assert!(command_names("python").is_empty());
    }

    #[test]
    fn the_lookup_is_case_insensitive() {
        // 工具 id 是 `node`，但用户可能敲 `Node` —— 存储目录名是稳定的 id，
        // 而"我认识这个工具"这件事不该因为大小写而不同。
        assert_eq!(shim_commands("Node").len(), 4);
        assert_eq!(shim_commands("NODE").len(), 4);
    }

    #[test]
    fn no_tool_exposes_the_same_command_name_twice() {
        // 两条命令同名会让后一条**静默覆盖**前一条（落盘路径相同），
        // 而用户看到的是"生成了 2 个文件"，实际上只有一个。
        for tool in SHIM_COMMANDS {
            let commands: Vec<&str> = tool.commands.iter().map(|c| c.command).collect();
            let mut unique = commands.clone();
            unique.sort_unstable();
            unique.dedup();
            assert_eq!(
                commands.len(),
                unique.len(),
                "`{}` 的表里有重名的命令：{commands:?}",
                tool.id
            );
        }
    }

    #[test]
    fn the_table_ids_are_ascii_lowercase_ids() {
        // 表里的 id 会进 `--json` 与存储目录名，所以它必须是稳定的形态。
        for tool in SHIM_COMMANDS {
            assert!(
                !tool.id.is_empty()
                    && tool
                        .id
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "`{}` 不是小写 ASCII 的工具 id",
                tool.id
            );
            assert!(!tool.commands.is_empty(), "`{}` 的表是空的", tool.id);
        }
    }
}
