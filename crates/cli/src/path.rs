//! `tuoen path` —— 读写用户级 `PATH`。
//!
//! # 为什么这一族必须存在
//!
//! `PATH` 是本项目里**唯一**一处「一次调用就能让整台机器所有命令失效」的地方
//! （`crates/platform/src/path.rs` 的模块文档里有那两个数字：`cmd.exe` 在 **8191**
//! 字符后完全忽略整条 `PATH`，`setx` 在 **1024** 处静默裁剪）。平台层已经把
//! 分析、计划、落盘全部实现并测过了，这一族做的是把那套能力**暴露给人**，
//! 而不是在旁边重写一份。
//!
//! # 三件事分得很开
//!
//! * [`PathCommand::Show`] —— **只读**。它一字节都不写，也不需要任何权限。
//! * [`PathCommand::Add`] / [`PathCommand::Remove`] —— 先出计划，再落盘；
//!   `--dry-run` 走的是**同一套** `plan_*`，只是不调 `apply`。
//! * 不带子命令的 `tuoen path` **等于 `tuoen path show`**：看是最安全的默认动作，
//!   而"敲了 `tuoen path` 就改了 `PATH`"是最不该发生的默认动作。
//!
//! # 为什么 `--dry-run` 不在 `show` 上
//!
//! `show` 本来就是干的。给一个只读命令加 `--dry-run` 会让人以为它平时会写东西。
//!
//! # 为什么 `--json` 在每一个叶子命令上
//!
//! 与 `shim` 那一族一致：`--json` 是我们公开输出契约的开关，它跟着**叶子命令**走
//! （`tuoen path show --json`、`tuoen path add C:\x --json`）。
//! **父级上没有 `--json`**：不带子命令时它已经被补成 `show`，所以
//! `tuoen path --json` 是用法的错 —— 而这比"父级与叶子两个开关谁说了算"
//! 更容易解释，也更容易测。

use clap::{Args, Subcommand};
use std::ffi::{OsStr, OsString};

/// 把不带子命令的 `tuoen path` 补成 `tuoen path show`。
///
/// # 为什么在进 clap 之前补，而不是在 clap 里配一个默认值
///
/// clap 4 **没有** `default_subcommand`。可选的替代方案有两个，都被否掉了：
///
/// * 把 `PathArgs::command` 写成 `Option<PathCommand>`，再在编排层 `unwrap_or(show)`
///   —— 那样"默认是什么"散落在两层（clap 给一个 `None`，编排层解释它），
///   而这正是本项目在别处一律避免的形状。
/// * 把 `show` 复制成一个独立分支 —— 于是 `tuoen path` 与 `tuoen path show`
///   是**两条**代码路径，它们迟早会岔开。
///
/// 补一个参数让两者成为**同一条**命令行：`tuoen path` 与 `tuoen path show` 在 clap
/// 眼里逐字相同，所以它们不可能走岔，而且这个默认值可以被单独测试
/// （本文件底部的 `bare_path_becomes_path_show`）。
///
/// # 判据
///
/// 第一个参数是 `path`，**且**它后面**什么都没有**。只要后面有东西 ——
/// 子命令、`-h` / `--help`、任何选项 —— 就一个字都不动。
///
/// **帮助开关必须留在父级**：`tuoen path --help` 要看的是"这一族有哪三个子命令"，
/// 而不是 `show` 一个子命令的细节。补成 `path show --help` 会让
/// `add` / `remove` 从帮助里消失 —— 那是一个**真的会误导人**的结果。
/// `-h` 与 `--help` 两个都认（clap 自己就是这么定义的）。
///
/// 别的选项（比如 `--json`）同样不触发补齐：`--json` 属于**叶子**命令
/// （与 `shim` 那一族一致），所以 `tuoen path --json` 是用法的错，
/// 而不是被悄悄当成 `path show --json`。
#[must_use]
pub fn default_path_subcommand(args: Vec<OsString>) -> Vec<OsString> {
    let is_path = args.get(1).is_some_and(|arg| arg == OsStr::new("path"));
    // 后面有东西就一个字都不动 —— 包括帮助开关。**只有"什么都没有"才补。**
    if !is_path || args.len() > 2 {
        return args;
    }
    let mut out = args;
    out.insert(2, OsString::from("show"));
    out
}

/// `tuoen path` 的子命令。
///
/// **不带子命令时走 [`PathCommand::Show`]** —— 由 [`default_path_subcommand`] 在
/// 进入 clap 之前把 `tuoen path` 补成 `tuoen path show`，所以两者解析出来的是
/// **同一个值**，而不是两条各写一遍的代码路径。
#[derive(Debug, Subcommand)]
pub enum PathCommand {
    /// 只读地报告 `PATH` 的现状（长度预算、重复、失效条目、遮蔽……）。
    #[command(long_about = r#"只读地报告 `PATH` 的现状。**这条命令一字节都不写。**

报告里的每一项都是**发现**，不是错误 —— 重复条目、指向不存在目录的条目、
硬编码了用户名的条目、被别的目录抢在前面的 shim，这些都只报告，**不自动清理**：
`PATH` 上的东西是用户的，我们只在他明确要求时改动它。

四件必须一起看的事：

1. **长度预算以「生效的 `PATH`」为准**，不是以某一个作用域为准。`cmd.exe` 看的是
   进程环境块里那一条，而它是机器级 + 用户级 + 进程注入拼起来的。超过 **8191**
   字符后 `cmd.exe` **完全忽略整条 `PATH`** —— 不是部分失效，是所有命令一起失效。
2. **机器级在前、用户级在后**，所以用户级工具永远输掉名字冲突。这就是 shim 存在的理由。
3. **进程注入项不在任何注册表里**（本机实测有 PowerShell 的 MSIX 别名），
   所以只看注册表会漏掉它们 —— 而它们真的占长度。
4. **遮蔽**：我们发布的哪几条命令会被别的目录抢先。  
   与上面第 2 条合起来看：`tuoen path add <shim 目录>` 才会让 shim 真的生效。"#)]
    Show(PathShowArgs),

    /// 把 `<dir>` 追加到**用户级** `PATH`（先出计划，再落盘）。
    #[command(long_about = r#"把 `<dir>` 追加到**用户级** `PATH` 的末尾。

**只写用户级**（`HKCU\Environment`，不需要提权）。机器级要提权，而 tuoen 不做
"顺手提权"这件事。

**追加在末尾，所以加目录永远不会把已有的条目挤到后面** —— 已经在前面的优先级不变。
反过来说，用户级永远排在机器级后面（平台事实），所以想让 shim 抢到名字，
加进去的必须是 shim 目录，靠的是那个目录里真有可执行文件。

**绝不调用 `setx`**：它会在 1024 字符处静默裁剪，并把值里所有 `%VAR%` 永久展开成
字面量。这里走的是一次整条写回（不是逐段改 —— 逐段改会有"`PATH` 暂时少了几个目录"
的中间状态），然后广播 `WM_SETTINGCHANGE`。

`--dry-run` 走的是**同一套**计划代码，只是不落盘。

**已经在里面了**是一个成功的空操作（退出码 0），而且**什么都不写**。

`;` 在 `PATH` 里是分隔符，**引号保护不了它**（Windows 先把整条值按 `;` 切开，
再决定要不要剥引号）。所以带 `;` 的目录名会被当场拒绝，而不是写进去之后
变成两条谁也不知道原本是一条的条目。"#)]
    Add(PathAddArgs),

    /// 从**用户级** `PATH` 里删掉 `<dir>` 的全部出现（先出计划，再落盘）。
    #[command(long_about = r#"从**用户级** `PATH` 里删掉 `<dir>` 的**全部**出现。

只动用户级：机器级要提权，而 tuoen 不做"顺手提权"这件事。

**本来就不在里面**是一个成功的空操作（退出码 0），并且会明说"什么都没有改" ——
"你要删的东西不在"不是失败。

**我们自己的 shim 目录不许删。** 用 shim 目录当参数时会被**拒绝**（退出码非 0）：
那等于把刚装好的命令从 `PATH` 上摘掉。真要那么做，先想清楚那些命令以后从哪里来。

写回是整条值一次写完，然后广播 `WM_SETTINGCHANGE`。**已经跑着的终端拿不到新环境**
（环境块是 `CreateProcess` 时复制的），这是平台限制，不是命令没生效。

`--dry-run` 走的是**同一套**计划代码，只是不落盘。"#)]
    Remove(PathRemoveArgs),
}

#[derive(Debug, Args)]
pub struct PathShowArgs {
    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PathAddArgs {
    /// 要加进用户级 `PATH` 的目录。**里面不能有 `;`**（它是条目分隔符）。
    pub dir: String,

    /// 只报计划，**什么都不写**。
    ///
    /// 它走的是与真写**同一套**计划代码（`plan_add`），区别只在最后不落盘。
    /// 漂移的预览比没有预览更危险。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct PathRemoveArgs {
    /// 要从用户级 `PATH` 里删掉的目录。**里面不能有 `;`**（它是条目分隔符）。
    pub dir: String,

    /// 只报计划，**什么都不写**。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use clap::{CommandFactory, Parser};

    use crate::cli::{Cli, Command};
    use crate::path::{
        PathAddArgs, PathCommand, PathRemoveArgs, PathShowArgs, default_path_subcommand,
    };

    /// 解析一次并取出 `path` 那一族的命令。
    ///
    /// **走的是与生产代码同一套参数补齐**（[`default_path_subcommand`]）：
    /// 命令行先原样拼出来再补齐 —— 所以这里测到的就是用户真的敲下去之后
    /// 会发生的事，`path_command(&[])` 解析的就是 `tuoen path show`。
    fn path_command(args: &[&str]) -> PathCommand {
        let mut full = vec![OsString::from("tuoen"), OsString::from("path")];
        full.extend(args.iter().map(OsString::from));
        match Cli::try_parse_from(default_path_subcommand(full))
            .expect("parse")
            .command
        {
            Command::Path(command) => command,
            other => panic!("应当是 path，实际：{other:?}"),
        }
    }

    /// 这一次子命令上的 `--json`。
    ///
    /// 只在测试里用：生产代码里每个 `run_*` 拿到的就是它自己的参数结构体，
    /// 直接读字段即可 —— 所以这里**不**给 `PathCommand` 挂一个只被测试用到的方法
    /// （那会让 `dead_code` 在发布构建里报警）。
    fn json_of(command: &PathCommand) -> bool {
        match command {
            PathCommand::Show(args) => args.json,
            PathCommand::Add(args) => args.json,
            PathCommand::Remove(args) => args.json,
        }
    }

    #[test]
    fn no_subcommand_means_show() {
        // **这一族最重要的一条默认值**：`tuoen path` 与 `tuoen path show` 必须是
        // 同一个东西 —— 否则"敲了就改了 PATH"迟早会发生。
        assert!(matches!(path_command(&[]), PathCommand::Show(_)));
        assert!(matches!(path_command(&["show"]), PathCommand::Show(_)));
        // 而"后面有东西"（这里是一个属于叶子的选项）**不**触发补齐：
        // 它是一条用法的错，不是被悄悄当成 `path show --json`。
        assert!(Cli::try_parse_from(["tuoen", "path", "--json"]).is_err());
    }

    #[test]
    fn add_and_remove_default_to_a_real_run_and_take_the_directory() {
        match path_command(&["add", r"C:\tools"]) {
            PathCommand::Add(PathAddArgs { dir, dry_run, json }) => {
                assert_eq!(dir, r"C:\tools");
                assert!(!dry_run, "默认必须是真写，不是演练");
                assert!(!json);
            }
            other => panic!("应当是 path add，实际：{other:?}"),
        }
        match path_command(&["remove", r"C:\tools", "--dry-run", "--json"]) {
            PathCommand::Remove(PathRemoveArgs { dir, dry_run, json }) => {
                assert_eq!(dir, r"C:\tools");
                assert!(dry_run);
                assert!(json);
            }
            other => panic!("应当是 path remove，实际：{other:?}"),
        }
    }

    #[test]
    fn both_arguments_are_required_so_a_typo_is_a_usage_error() {
        // `tuoen path add` 什么都不带必须是用法的错，而不是"加个空目录"。
        assert!(Cli::try_parse_from(["tuoen", "path", "add"]).is_err());
        assert!(Cli::try_parse_from(["tuoen", "path", "remove"]).is_err());
    }

    #[test]
    fn show_takes_json_and_nothing_else() {
        // `--dry-run` 只属于会写的那两条：给只读命令加它会让它看起来平时会写东西。
        assert!(Cli::try_parse_from(["tuoen", "path", "show", "--dry-run"]).is_err());
        assert!(matches!(
            path_command(&["show", "--json"]),
            PathCommand::Show(PathShowArgs { json: true })
        ));
        assert!(matches!(
            path_command(&["show"]),
            PathCommand::Show(PathShowArgs { json: false })
        ));
    }

    #[test]
    fn json_lives_on_the_leaves_only_so_the_parent_never_swallows_it() {
        // `--json` 跟着**叶子**命令走（与 `shim` 那一族一致），父级上没有它 ——
        // 所以 `tuoen path --json` 是用法的错，而不是被补齐成 `path show --json`。
        // 这条断言同时钉住"只有一个归属"和"补齐不会替用户猜"。
        assert!(
            Cli::try_parse_from(["tuoen", "path", "--json"]).is_err(),
            "父级上不该有 --json，也不该被补齐掩盖成叶子的参数"
        );
        for args in [
            vec!["show", "--json"],
            vec!["add", r"C:\x", "--json"],
            vec!["remove", r"C:\x", "--json"],
        ] {
            assert!(
                json_of(&path_command(&args)),
                "{args:?} 上的 --json 必须生效"
            );
        }
        // 没写就是不写。
        assert!(!json_of(&path_command(&["show"])));
        assert!(!json_of(&path_command(&["add", r"C:\x"])));
    }

    #[test]
    fn bare_path_becomes_path_show_and_nothing_else_is_touched() {
        use std::ffi::OsString;

        fn os(args: &[&str]) -> Vec<OsString> {
            args.iter().map(OsString::from).collect()
        }

        // 这一族唯一的默认值：不带子命令 = `show`。
        assert_eq!(
            default_path_subcommand(os(&["tuoen", "path"])),
            os(&["tuoen", "path", "show"])
        );
        // **帮助开关留在父级**：`path --help` 要看的是"有哪三个子命令"，
        // 补成 `path show --help` 会让 `add` / `remove` 从帮助里消失。
        for help in ["-h", "--help"] {
            assert_eq!(
                default_path_subcommand(os(&["tuoen", "path", help])),
                os(&["tuoen", "path", help]),
                "`path {help}` 必须留在父级"
            );
        }
        // 显式给了子命令就一个字都不动。
        for args in [
            vec!["tuoen", "path", "show"],
            vec!["tuoen", "path", "show", "--json"],
            vec!["tuoen", "path", "add", r"C:\x"],
            vec!["tuoen", "path", "remove", r"C:\x", "--dry-run"],
            // `--json` 属于叶子，所以它**不**触发补齐（那会是一个用法的错）。
            vec!["tuoen", "path", "--json"],
        ] {
            assert_eq!(default_path_subcommand(os(&args)), os(&args), "{args:?}");
        }
        // 别的命令、以及 `path` 出现在别的位置时都不许动。
        for args in [
            vec!["tuoen", "detect"],
            vec!["tuoen", "list"],
            vec!["tuoen", "shim", "path"],
            vec!["tuoen"],
            vec![],
        ] {
            assert_eq!(default_path_subcommand(os(&args)), os(&args), "{args:?}");
        }
        // **补上之后必须真的解析成 `show`** —— 否则上面那些断言只是在比字符串。
        assert!(matches!(
            path_command(&[]),
            PathCommand::Show(PathShowArgs { .. })
        ));
    }

    #[test]
    fn cli_definition_is_still_valid_after_adding_path() {
        Cli::command().debug_assert();
    }
}
