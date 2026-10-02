//! `tuoen doctor` 的参数。
//!
//! **长帮助不在这里**：那几件必须说清的事（这条命令只读 / 为什么**没有** `--fix` /
//! 为什么发现了 error 退出码还是 0）写在 [`crate::cli::Command::Doctor`] 上，
//! 与 `capture` / `detect` 的位置一致 —— 参数结构体里只留形状与取值，
//! 帮助文本只有一处。
//!
//! # 为什么没有 `--fix`
//!
//! 票据明确写着"绝不修改任何东西、不提供 `--fix`"。这不是"这一版还没做"，
//! 而是**这一票的判据**：`doctor` 的结论要能被信，前提是它**没有动机**把结论
//! 做得好看 —— 一个能顺手改机器的体检工具，会让人分不清"报告是症状"还是
//! "报告是它自己动过的痕迹"。改机器是 `restore` 那一票的事，而它还不存在。
//!
//! # 为什么没有 `--fail-on` / `--strict`
//!
//! 退出码的取舍写在 [`crate::cli::Command::Doctor`] 上（0 = 体检跑完了，
//! 哪怕发现了 error）。既然如此，"发现了 error 就返回非 0"就必须由**消费者**
//! 决定，而它的判据是 `--json` 里的 `counts.error` —— 一个**不用解析人话**、
//! 也不用在每次体检里重新定义"多严重才算严重"就能读到的数字。
//!
//! 再加一个 `--fail-on <severity>` 会带来第二个问题：它把"发现了什么"与
//! "这一次要不要红"混进同一条命令行，于是同一次体检在 CI 与在手边会**长得不一样**
//! （人会下意识加上 `--strict` 才敢跑）。本项目一贯的立场是：**机器读 `--json`**，
//! 严重度本身就是数据。

use clap::Args;

/// `tuoen doctor` 的参数。
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,

    /// 不跑"全局包前缀在哪"这类探测（`npm config get prefix`）。
    #[arg(long)]
    pub no_probe: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::DoctorArgs;
    use crate::cli::{Cli, Command};

    #[test]
    fn a_bare_doctor_writes_human_output_and_probes_by_default() {
        let cli = Cli::try_parse_from(["tuoen", "doctor"]).expect("parse");
        match cli.command {
            Command::Doctor(DoctorArgs { json, no_probe }) => {
                assert!(!json, "默认是人类输出（中文优先）");
                // **探测默认开**：它是 `error` 级检查（全局包前缀在版本目录里）
                // 的唯一输入 —— 默认关掉等于默认少一类结论，而报告不会说少了什么。
                assert!(!no_probe, "默认要跑前缀探测");
            }
            other => panic!("应当是 doctor，实际：{other:?}"),
        }
    }

    #[test]
    fn both_flags_are_accepted() {
        let cli = Cli::try_parse_from(["tuoen", "doctor", "--json", "--no-probe"]).expect("parse");
        match cli.command {
            Command::Doctor(DoctorArgs { json, no_probe }) => {
                assert!(json);
                assert!(no_probe);
            }
            other => panic!("应当是 doctor，实际：{other:?}"),
        }
    }

    #[test]
    fn fix_is_not_a_flag_and_that_is_deliberate() {
        // 一条**管线**上的断言：`--fix` 必须被 clap 当场拒掉（退出码 2），
        // 而不是被当成未来的开关静默接受。任何"以后再加"的实现都会让这条红 ——
        // 而那正是我们要的提醒（这一票的判据是"绝不修改任何东西"）。
        for flag in ["--fix", "--strict", "--fail-on"] {
            let Err(error) = Cli::try_parse_from(["tuoen", "doctor", flag]) else {
                panic!("`{flag}` 必须被拒，但 clap 接受了它");
            };
            assert_eq!(error.exit_code(), 2, "{flag}：{error}");
        }
    }
}
