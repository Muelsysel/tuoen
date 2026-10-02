//! 命令行参数定义。
//!
//! **中文优先**：`about` / `help` 文本是中文。但**子命令名、参数名、`--json` 的键与取值
//! 一律英文且不本地化** —— 否则脚本与未来的 GUI 会被界面语言绑死（决策 35）。

use clap::{Args, Parser, Subcommand};

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
        }
    }

    #[test]
    fn list_defaults_to_human_output() {
        let cli = Cli::try_parse_from(["tuoen", "list"]).expect("parse");
        match cli.command {
            Command::List(args) => assert!(!args.json),
        }
    }

    #[test]
    fn no_subcommand_is_a_usage_error() {
        assert!(Cli::try_parse_from(["tuoen"]).is_err());
    }
}
