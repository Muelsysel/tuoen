//! `tuoen shell` 与 `tuoen auto` 的参数。
//!
//! **长帮助不在这里**：那几件必须说清的事（为什么 `shell` 永远可用而 `auto` 要多一道
//! 信任门 / 为什么 `--json` 必须配 `--dry-run` / 为什么这里**没有** `--ignore-lock`）
//! 写在 [`crate::cli::Command::Shell`] 与 [`crate::cli::Command::Auto`] 上 ——
//! 参数结构体里只留形状与取值，帮助文本只有一处（与 `doctor.rs` / `capture.rs` 一致）。
//!
//! # `--json` 必须配 `--dry-run`（决策 123）
//!
//! 这一条由 clap 的 `requires` 保证：`tuoen shell --json` 是**用法错误**，
//! 退出码 **2**，而且是在**解析阶段**就被拒掉 —— 不是"跑起来之后才发现不该这么用"。
//!
//! 判据（一字不差）：`--json requires --dry-run`。
//!
//! 理由不是洁癖：交互式子 shell 与 JSON 共用 stdout 是**必然打架**的 ——
//! 子进程会往同一个 fd 上写东西（`cmd.exe` 的提示符、`dir` 的输出、PowerShell 的
//! 启动横幅）。把"计划"与"执行"分开之后，JSON 契约就永远是完整的：
//! `--json` 只描述计划，`--dry-run` 保证它不会被任何子进程的字节污染。
//!
//! # `--shell` 的默认值
//!
//! 默认 `cmd`。理由不是偏好：`cmd.exe` 在每一个 Windows 上都在同一个位置，
//! 而 `powershell.exe` 可能被组策略、执行策略或"只装了 PowerShell 7"这类情况
//! 挪走 —— 一个**默认**值不该是那条需要额外前提的路径。要 PowerShell 就显式写出来。
//!
//! # 两个命令共用一套参数
//!
//! [`AutoArgs`] 与 [`ShellArgs`] 是**同一个形状的两个名字**：决策 121 要求
//! `auto` 与 `shell` 共用同一个计划构造器，差别只有那道信任门。参数上分家是
//! 第一步漂移 —— 漂移的表现是"`auto` 切了、`shell` 没切"。

use clap::{Args, ValueEnum};
use tuoen_core::pin::ShellKind;

/// `--shell` 的取值。
///
/// **为什么不在 clap 上直接用 [`ShellKind`]**：那是 core 的类型，而 clap 的
/// `ValueEnum` 是**界面**的一部分（取值名、帮助文本、大小写规则都在这里）。
/// 让界面直接绑到 core 的类型上，意味着 core 加一个变体就会悄悄改掉命令行契约。
/// 这一层薄映射的代价是三行，收益是"命令行取值"与"内部枚举"能被分别改动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ShellChoice {
    /// `cmd.exe`（默认）。
    Cmd,
    /// `powershell.exe`（Windows PowerShell 5.1，带 `-NoProfile`）。
    Powershell,
}

impl ShellChoice {
    /// 映射到 core 的 [`ShellKind`]。
    #[must_use]
    pub const fn kind(self) -> ShellKind {
        match self {
            Self::Cmd => ShellKind::Cmd,
            Self::Powershell => ShellKind::PowerShell,
        }
    }
}

/// `tuoen shell` 的参数。
#[derive(Debug, Args)]
pub struct ShellArgs {
    /// 起哪一种 shell。
    #[arg(long, value_enum, default_value_t = ShellChoice::Cmd)]
    pub shell: ShellChoice,

    /// 不进入交互式 shell，只跑这一条命令（用于测试与脚本）。
    #[arg(long, value_name = "COMMAND")]
    pub exec: Option<String>,

    /// 只打印计划，**不启动任何子进程、不写任何文件**。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。**必须与 `--dry-run` 一起用**。
    #[arg(long, requires = "dry_run")]
    pub json: bool,
}

/// `tuoen auto` 的参数。
///
/// 形状与 [`ShellArgs`] 完全一致 —— 决策 121：两者共用同一个计划构造器，
/// 差别只有"这个目录被信任了吗"那一道门。
#[derive(Debug, Args)]
pub struct AutoArgs {
    /// 起哪一种 shell。
    #[arg(long, value_enum, default_value_t = ShellChoice::Cmd)]
    pub shell: ShellChoice,

    /// 不进入交互式 shell，只跑这一条命令（用于测试与脚本）。
    #[arg(long, value_name = "COMMAND")]
    pub exec: Option<String>,

    /// 只打印计划，**不启动任何子进程、不写任何文件**。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。**必须与 `--dry-run` 一起用**。
    #[arg(long, requires = "dry_run")]
    pub json: bool,
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{AutoArgs, ShellArgs, ShellChoice};
    use crate::cli::{Cli, Command};

    #[test]
    fn a_bare_shell_defaults_to_cmd_and_human_output() {
        let cli = Cli::try_parse_from(["tuoen", "shell"]).expect("parse");
        match cli.command {
            Command::Shell(ShellArgs {
                shell,
                exec,
                dry_run,
                json,
            }) => {
                assert_eq!(
                    shell,
                    ShellChoice::Cmd,
                    "默认是 cmd（最不需要前提的那条路）"
                );
                assert!(exec.is_none(), "不带 --exec 才是交互式");
                assert!(!dry_run);
                assert!(!json, "默认是人类输出（中文优先）");
            }
            other => panic!("应当是 shell，实际：{other:?}"),
        }
    }

    #[test]
    fn json_without_dry_run_is_a_usage_error_with_exit_code_two() {
        // **这一票的判据之一**（决策 123）：`--json requires --dry-run`。
        // 交互式子 shell 与 JSON 共用 stdout 必然打架，所以这不是"以后再说"，
        // 而是**当场拒掉**。任何"先接受、运行时再忽略"的实现都会让这条红。
        for args in [
            vec!["tuoen", "shell", "--json"],
            vec!["tuoen", "auto", "--json"],
        ] {
            let Err(error) = Cli::try_parse_from(&args) else {
                panic!("`{args:?}` 必须被拒，但 clap 接受了它");
            };
            assert_eq!(error.exit_code(), 2, "{args:?}：{error}");
            let rendered = error.to_string();
            assert!(
                rendered.contains("--dry-run"),
                "拒掉的时候要说清缺的是什么：{rendered}"
            );
        }
    }

    #[test]
    fn json_with_dry_run_is_accepted_for_both_commands() {
        let cli = Cli::try_parse_from(["tuoen", "shell", "--json", "--dry-run"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Shell(ShellArgs { json: true, .. })
        ));

        let cli = Cli::try_parse_from(["tuoen", "auto", "--dry-run", "--json"]).expect("parse");
        assert!(matches!(
            cli.command,
            Command::Auto(AutoArgs { json: true, .. })
        ));
    }

    #[test]
    fn exec_is_a_single_command_not_a_positional_list() {
        // `--exec` 收的是**一条**命令（整条 `a && b` 是 cmd.exe 自己解析的），
        // 所以多写一个位置参数必须是用法错误 —— 静默忽略会让"我传了两个命令"
        // 变成"只有一个跑了，另一个不见了"。
        let cli =
            Cli::try_parse_from(["tuoen", "shell", "--exec", "echo a && echo b"]).expect("parse");
        match cli.command {
            Command::Shell(ShellArgs {
                exec: Some(cmd), ..
            }) => {
                assert_eq!(cmd, "echo a && echo b", "整条命令是一个 argv 元素");
            }
            other => panic!("应当是 shell，实际：{other:?}"),
        }
        assert!(Cli::try_parse_from(["tuoen", "shell", "--exec", "a", "b"]).is_err());
    }

    #[test]
    fn the_shell_choice_maps_onto_the_core_kind() {
        assert_eq!(ShellChoice::Cmd.kind(), tuoen_core::pin::ShellKind::Cmd);
        assert_eq!(
            ShellChoice::Powershell.kind(),
            tuoen_core::pin::ShellKind::PowerShell
        );
    }

    #[test]
    fn there_is_no_ignore_lock_switch_and_that_is_deliberate() {
        // 决策 116：锁与声明不一致时**停下来**，不给 `--ignore-lock` 之类的开关
        // （"先有需求再加，加容易、去难"）。一条管线上的断言：它必须被 clap 拒掉。
        for flag in ["--ignore-lock", "--force", "--no-lock"] {
            for command in ["shell", "auto", "lock"] {
                let Err(error) = Cli::try_parse_from(["tuoen", command, flag]) else {
                    panic!("`{command} {flag}` 必须被拒");
                };
                assert_eq!(error.exit_code(), 2, "{command} {flag}：{error}");
            }
        }
    }
}
