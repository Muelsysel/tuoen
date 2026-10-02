//! `tuoen detect` —— 只读地读出这台机器上装了哪些开发工具。
//!
//! **这一票的产出是"证据"不是"功能"**：`detect` 只报告，不修任何东西。
//! 它的价值在于**每条记录都带来源与置信度** —— 用户需要知道
//! "这条能不能在新机器上自动重建"，以及"这条到底是不是真的存在"。
//!
//! 七层置信度不是装饰：本机的 `python` 就是被一个 0 字节的 App Execution Alias
//! 抢走的，`Get-Command python` 成功而它根本不是文件。把那条与真实的
//! `Python312\python.exe` 并列展示、且不区分，等于给出错误答案。

use clap::Args;

#[derive(Debug, Args)]
pub struct DetectArgs {
    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,

    /// 不探测版本（快得多，用于 `doctor` 这类只关心结构的场景）。
    #[arg(long)]
    pub no_version: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command};

    #[test]
    fn detect_parses_with_both_flags() {
        let cli =
            Cli::try_parse_from(["tuoen", "detect", "--json", "--no-version"]).expect("parse");
        match cli.command {
            Command::Detect(args) => {
                assert!(args.json);
                assert!(args.no_version);
            }
            other => panic!("应当是 detect，实际：{other:?}"),
        }
    }

    #[test]
    fn detect_defaults_to_human_and_probing() {
        let cli = Cli::try_parse_from(["tuoen", "detect"]).expect("parse");
        match cli.command {
            Command::Detect(args) => {
                assert!(!args.json);
                assert!(
                    !args.no_version,
                    "默认要探测版本 —— 版本是这份报告的主要信息"
                );
            }
            other => panic!("应当是 detect，实际：{other:?}"),
        }
    }
}
