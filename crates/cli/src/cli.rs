//! 命令行参数定义。
//!
//! **中文优先**：`about` / `help` 文本是中文。但**子命令名、参数名、`--json` 的键与取值
//! 一律英文且不本地化** —— 否则脚本与未来的 GUI 会被界面语言绑死（决策 35）。

use clap::{Args, Parser, Subcommand};

use crate::catalog::CatalogCommand;
use crate::detect::DetectArgs;

/// 拓境 — 让你的 Windows 开发环境可搬运、可复现。
#[derive(Debug, Parser)]
#[command(
    name = "tuoen",
    version,
    about = "拓境 — 让你的 Windows 开发环境可搬运、可复现。",
    long_about = r#"tuoen（拓境）是 Windows 开发环境的捕获与还原器。

它不是一个「再装一遍」的包管理器：它把你整台机器的开发状态读成机器可读形式，
让你能在换电脑时重建它、在搬运前先看清哪里是坏的。

文档：https://github.com/Muelsysel/tuoen"#,
    subcommand_required = true,
    arg_required_else_help = true,
    disable_help_subcommand = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// 列出由 tuoen 管理的工具。
    #[command(long_about = r#"列出由 tuoen 管理的工具。

注意：这里只列出 **tuoen 自己管理** 的工具。
检测整机已安装的工具请用 `tuoen detect`；
整机状态快照请用 `tuoen capture`。"#)]
    List(ListArgs),

    /// 查看 tuoen 认识哪些工具、它们的许可证，以及某个版本能不能装。
    #[command(subcommand)]
    Catalog(CatalogCommand),

    /// 只读地读出这台机器上装了哪些开发工具（带来源与置信度）。
    #[command(long_about = r#"只读地读出这台机器上装了哪些开发工具。

**这条命令什么都不改。** 它把六个来源的发现合并成一份报告，
每条记录都带**来源**（怎么被发现的）与**置信度**（有多可信）。

七层置信度：
  managed             由 tuoen 自己安装
  executable          在 PATH 上解析到，文件真实存在且能问到版本
  registered          注册表声称已装且安装目录真的在，但它不在 PATH 上
  manager-owned       由第三方版本管理器管理（只读采纳，tuoen 不会去动它）
  directory-only      发现了一个安装目录，但它不在 PATH 也不在任何注册表里
  registered-missing  注册表声称已安装，但文件不存在 —— 幽灵条目
  alias-ghost         0 字节的 App Execution Alias（Test-Path 通过，但它不是文件）

**同一个工具出现多行是正常的，而且是有信息的**：
本机的 Java 就是「PATH 上 java 来自 Oracle 1.8.0_491，而 javac 来自 Amazon 1.8.0_492」
—— 这是真实的分裂，不是重复。`#` 列就是用来一眼看出这件事的。"#)]
    Detect(DetectArgs),
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // clap 的自我校验：能抓出重复的短参数、空的 about 等定义错误。
        Cli::command().debug_assert();
    }

    #[test]
    fn list_accepts_json_flag() {
        let cli = Cli::try_parse_from(["tuoen", "list", "--json"]).expect("parse");
        match cli.command {
            Command::List(args) => assert!(args.json),
            other => panic!("应当是 list，实际：{other:?}"),
        }
    }

    #[test]
    fn list_defaults_to_human_output() {
        let cli = Cli::try_parse_from(["tuoen", "list"]).expect("parse");
        match cli.command {
            Command::List(args) => assert!(!args.json),
            other => panic!("应当是 list，实际：{other:?}"),
        }
    }

    #[test]
    fn no_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["tuoen"]).is_err());
    }

    #[test]
    fn detect_is_a_top_level_command_not_under_catalog() {
        // `detect` 与 `catalog` 是两件事：前者读**这台机器**，后者读**我们的目录**。
        // 把 detect 塞进 catalog 会让"我机器上装了什么"看起来像"我们支持什么"。
        let cli = Cli::try_parse_from(["tuoen", "detect"]).expect("parse");
        assert!(matches!(cli.command, Command::Detect(_)));
        assert!(Cli::try_parse_from(["tuoen", "catalog", "detect"]).is_err());
    }
}
