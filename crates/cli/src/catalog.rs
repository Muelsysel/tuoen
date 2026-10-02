//! `tuoen catalog` —— 查看 tuoen 认识哪些工具、每个工具的许可证是什么、
//! 以及某个具体版本能不能装。
//!
//! **ticket #3 的验收要求"这一票要能回答这三个问题，而不需要真的下载任何东西"**，
//! 所以这一族子命令就是那三个问题的出口。

use clap::{Args, Subcommand};

/// 参数容器。存在的理由：clap 的 `#[command(subcommand)]` 不能直接贴在
/// 枚举变体上，需要一个 struct 承接。
#[derive(Debug, Args)]
pub struct CatalogArgs {
    #[command(subcommand)]
    pub command: CatalogCommand,
}

#[derive(Debug, Subcommand)]
pub enum CatalogCommand {
    /// 列出所有已知工具及其许可证结论。
    #[command(long_about = r#"列出所有已知工具及其许可证结论。

许可证结论有四档：allowed（可再分发）/ metadata-only（只镜像元数据）/
conditional（有条件）/ prohibited（不可再分发）。

**prohibited 的条目不是脏数据** —— 它们是对用户的说明：
「我们知道它存在，但你不能通过 tuoen 装它，原因如下。」"#)]
    List(JsonFlag),

    /// 显示一个工具的详情：别名、上游、所有版本与各自的许可证结论。
    Show(ShowArgs),

    /// 回答「某个版本能不能装」，并在被拒时给出**具体原因**。
    Check(CheckArgs),
}

#[derive(Debug, Args)]
pub struct JsonFlag {
    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowArgs {
    /// 工具 id 或别名。
    pub tool: String,

    /// 输出稳定的 JSON。
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CheckArgs {
    /// 工具 id 或别名。
    pub tool: String,

    /// 版本约束（精确版本，或 `24` / `8` 这样的前缀）。
    pub version: String,

    /// 目标平台。默认 `windows-x64`。
    #[arg(long, default_value = "windows-x64")]
    pub platform: String,

    /// 输出稳定的 JSON。
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use crate::catalog::CatalogCommand;
    use crate::cli::Cli;

    #[test]
    fn catalog_list_parses() {
        let cli = Cli::try_parse_from(["tuoen", "catalog", "list"]).expect("parse");
        assert!(matches!(cli.command, crate::cli::Command::Catalog(_)));
    }

    #[test]
    fn catalog_check_defaults_to_windows_x64() {
        let cli = Cli::try_parse_from(["tuoen", "catalog", "check", "node", "24"]).expect("parse");
        match cli.command {
            crate::cli::Command::Catalog(CatalogCommand::Check(check)) => {
                assert_eq!(check.platform, "windows-x64");
                assert_eq!(check.tool, "node");
                assert_eq!(check.version, "24");
            }
            other => panic!("应当是 catalog check，实际：{other:?}"),
        }
    }

    #[test]
    fn catalog_show_requires_a_tool() {
        assert!(Cli::try_parse_from(["tuoen", "catalog", "show"]).is_err());
    }

    #[test]
    fn cli_definition_is_still_valid_after_adding_catalog() {
        Cli::command().debug_assert();
    }
}
