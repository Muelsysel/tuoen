//! `tuoen shim` —— 把工具的命令以**真 `.exe`** 的形式放到 `PATH` 上。
//!
//! # 为什么是 shim，而不是改 `PATH` 顺序
//!
//! 进程的 `PATH` 是「机器条目在前、用户条目在后」（`docs/DESIGN.md` §2.1 实测），
//! 所以用户级安装**永远输掉名字冲突** —— 想抢回 `node` 这个名字，只有一个办法：
//! 在 `PATH` 靠前的位置放一个**真的可执行文件**（决策 10）。
//!
//! # 为什么只发 `.exe`
//!
//! `.cmd` / `.ps1` shim 在 Node ≥18.20.2 / 20.12.2 / 21.7.3 之后**无法被 spawn**
//! （CVE-2024-27980），而在那之前它们也只是一层 shell —— 参数的引号、`%VAR%`、
//! `&` 都要再过一遍 cmd 的解析器。所以 shim **只发 `.exe`**，
//! 目标路径在生成时**烘进二进制**，运行时一个文件都不读。
//!
//! # 为什么切版本不用重新生成 shim
//!
//! 因为 shim 指向的是 `<store>/<tool>/current`（一个 junction），**不是**某个具体版本目录。
//! `tuoen use` 只翻转那个链接，`PATH` 上那批 `.exe` 一动不动。
//! 这就是决策 11 的全部意义：**换版本与换 shim 是两件事**。

use clap::{Args, Subcommand};

#[derive(Debug, Subcommand)]
pub enum ShimCommand {
    /// 为一个工具的每个命令生成 shim（`.exe`）。
    #[command(
        long_about = r#"为一个工具的每个命令生成 shim（`.exe`），放进 shim 目录。

**shim 指向的是 `<store>/<工具>/current`，不是某个具体版本目录。**
所以切版本（`tuoen use`）**不必重新生成 shim** —— 那一次翻转只动链接，
`PATH` 上这批 `.exe` 一动不动（`docs/DESIGN.md` 决策 11）。

反过来说：`tuoen shim add node@24` 里那个版本号**不是**"把 shim 钉到这个版本"，
而是"确认这个版本真的装过"。（真要切换，用 `tuoen use node 24.19.0`；
如果指定的版本不是当前生效版本，输出会明确告诉你这件事。）

每个工具要暴露哪些命令是一张**实测过的表**（`tuoen-core` 里维护）：
node 暴露 `node` / `npm` / `npx` / `corepack` 四条。**没有实测过启动器的工具会拒绝**，
而不是猜一条路径 —— 猜错的症状是"shim 生成成功、敲命令时才发现目标不存在"。

`npm` / `npx` / `corepack` 在 Windows 上只有 `.cmd` 启动器，而 shim 拒绝脚本目标，
所以它们走「`node.exe` + 指向 `cli.js` 的前缀参数」这条路（实测与 `.cmd` 输出逐字节一致）。

一条命令失败**不会回滚已经成功的那几条**：shim 是逐条落盘的，
而"把刚生成好的 3 条删掉"只会让状态更难解释。失败会明确报出来，退出码非 0。"#
    )]
    Add(ShimAddArgs),

    /// 从 shim 目录里删掉若干个 shim。
    #[command(long_about = r#"从 shim 目录里删掉若干条 shim（`<名字>.exe`）。

**只删 tuoen 自己生成的**：删之前会读一遍文件，里面有 shim 的槽位魔数才动手。
一个不属于我们的文件出现在 shim 目录里，多半意味着别的东西也在这条 `PATH` 上放文件 ——
那件事值得先看一眼，而不是被我们顺手删掉。

名字是**不带扩展名**的命令名（`node`）；`node.exe` 也收，因为我们所有的 shim
都是 `.exe`，结尾的 `.exe` 只可能是扩展名。

已经删掉的那几条**不会因为后面某一条失败而恢复**，退出码非 0 表示"有名字没删成"。"#)]
    Remove(ShimRemoveArgs),

    /// 列出 shim 目录里的 shim，以及它们各自指向哪里。
    #[command(
        long_about = r#"列出 shim 目录里的 shim（`<名字>.exe`），以及它们各自指向哪里。

指向是**从文件里解出来的**（shim 把目标路径烘进了二进制），不是从任何配置文件读的 ——
所以它反映的是磁盘上的事实。

解不出来的文件**不会让命令失败**，只会被标成「不是 tuoen 的 shim 或已损坏」：
一个坏文件是**发现**，不是错误。"#
    )]
    List(ShimListArgs),

    /// 打印 shim 目录的绝对路径。
    #[command(
        long_about = r#"打印 shim 目录的绝对路径。人类输出是**恰好一行**，不加引号、不加前缀。

存在的理由是脚本：`export PATH=$(tuoen shim path)` 这类用法要的是一个能直接用的路径，
而不是一句"shim 目录是 ……"。

**这条命令什么都不改。**"#
    )]
    Path(ShimPathArgs),
}

#[derive(Debug, Args)]
pub struct ShimAddArgs {
    /// 工具 id 或别名，可带版本：`node` / `node@24.19.0`。
    ///
    /// 不带版本时用**当前激活版本**（`tuoen use` 生效的那个）。
    /// 带版本时只用来确认"这个版本真的装过"—— shim 指向的仍然是 `current`。
    pub spec: String,

    /// 只报计划，**什么都不写**。
    ///
    /// 它走的是与真装**同一套解析**（工具、版本、模板、每条命令的目标），
    /// 区别只在最后不落盘：漂移的预览比没有预览更危险（决策 20）。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShimRemoveArgs {
    /// 要删掉的命令名（不带扩展名；`node.exe` 也收）。可以给多个。
    #[arg(required = true)]
    pub names: Vec<String>,

    /// 输出稳定的 JSON。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShimListArgs {
    /// 输出稳定的 JSON。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShimPathArgs {
    /// 输出稳定的 JSON（`{"path": "…"}`）。
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use crate::cli::{Cli, Command};
    use crate::shim::ShimCommand;

    #[test]
    fn shim_add_takes_an_at_spec_and_defaults_to_a_real_run() {
        match Cli::try_parse_from(["tuoen", "shim", "add", "node@24.19.0"])
            .expect("parse")
            .command
        {
            Command::Shim(ShimCommand::Add(args)) => {
                assert_eq!(args.spec, "node@24.19.0");
                assert!(!args.dry_run, "默认必须是真写，不是演练");
                assert!(!args.json);
            }
            other => panic!("应当是 shim add，实际：{other:?}"),
        }
    }

    #[test]
    fn shim_remove_requires_at_least_one_name() {
        assert!(Cli::try_parse_from(["tuoen", "shim", "remove"]).is_err());
        match Cli::try_parse_from(["tuoen", "shim", "remove", "npm", "npx"])
            .expect("parse")
            .command
        {
            Command::Shim(ShimCommand::Remove(args)) => {
                assert_eq!(args.names, vec!["npm".to_owned(), "npx".to_owned()]);
            }
            other => panic!("应当是 shim remove，实际：{other:?}"),
        }
    }

    #[test]
    fn shim_list_and_path_take_only_json() {
        assert!(matches!(
            Cli::try_parse_from(["tuoen", "shim", "list", "--json"])
                .expect("parse")
                .command,
            Command::Shim(ShimCommand::List(args)) if args.json
        ));
        assert!(matches!(
            Cli::try_parse_from(["tuoen", "shim", "path"])
                .expect("parse")
                .command,
            Command::Shim(ShimCommand::Path(args)) if !args.json
        ));
        // `shim` 自己不带子命令是用法错误，不是"列出全部"。
        assert!(Cli::try_parse_from(["tuoen", "shim"]).is_err());
    }

    #[test]
    fn shim_is_a_top_level_family() {
        // shim 会**往 PATH 上放可执行文件**，与 `catalog` 那种只读目录的命令不是一类。
        assert!(Cli::try_parse_from(["tuoen", "catalog", "shim"]).is_err());
    }

    #[test]
    fn cli_definition_is_still_valid_after_adding_shim() {
        Cli::command().debug_assert();
    }
}
