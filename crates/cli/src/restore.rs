//! `tuoen restore <tuoen.d 目录>` 的参数形状（票据 #16）。
//!
//! ## 为什么默认是"只出计划"（决策 150）
//!
//! `restore` 是本项目里后果最重的命令：它会装工具、写用户级环境变量、重建用户级
//! `PATH`。一个"跑起来就改机器"的默认值违背规矩 3（先出计划，再落盘），而且它
//! 要动的恰恰是**别的进程此刻正在读**的那份环境。
//!
//! 所以：**不带开关 = 只出计划**，`--apply` 才是"动手"。`--dry-run` 是默认行为的
//! **显式别名** —— 它的存在是为了让脚本能把自己想干什么写出来，而不是为了改变
//! 行为。两者同时出现是矛盾，由 clap 在**解析期**拒绝（退出 2），不留给运行时。
//!
//! ## `--only` 的取值表（决策 159）
//!
//! 取值表由 `SectionId::ALL` 生成 —— CLI **不另抄一份 slug 表**。抄一份的下场是
//! core 加一个 section 时，`--only` 会悄悄拒绝它，而错误消息里还列着旧的四项。

use std::path::PathBuf;

use clap::Args;
use tuoen_core::restore::SectionId;

/// 默认的 `<dir>`。与 `capture --out` 的默认值同一个名字。
pub const DEFAULT_DIR: &str = "tuoen.d";

/// `tuoen restore` 的全部参数。
#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// `tuoen.d` 目录（`tuoen capture` 写出来的那个）。
    #[arg(default_value = DEFAULT_DIR)]
    pub dir: PathBuf,

    /// 只做这些 section（可重复）。取值就是四个 slug：tools / path / env / wsl。
    #[arg(long = "only", value_name = "SECTION", value_parser = parse_section)]
    pub only: Vec<SectionId>,

    /// 真的动手（装工具、写用户级环境变量、重建用户级 PATH）。
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,

    /// 只出计划 —— **这就是默认行为**，这个开关只是把它说出来。
    #[arg(long)]
    pub dry_run: bool,

    /// path section 里把**本机自己的**健康问题（重复 / 失效 / 空条目）也一起应用。
    #[arg(long)]
    pub with_fix: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

/// `--only` 的取值解析。取值表来自 `SectionId::ALL`，错误消息里也列它。
fn parse_section(text: &str) -> Result<SectionId, String> {
    SectionId::parse(text).ok_or_else(|| {
        let all: Vec<&str> = SectionId::ALL.iter().map(|s| s.as_str()).collect();
        format!("`{text}` 不是一个 section；取值是 {}", all.join(" / "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn restore(args: &[&str]) -> RestoreArgs {
        let mut argv = vec!["tuoen", "restore"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).expect("参数应当解析成功").command {
            crate::cli::Command::Restore(args) => args,
            other => panic!("应当解析成 restore，实际是 {other:?}"),
        }
    }

    #[test]
    fn the_directory_defaults_to_tuoen_d() {
        assert_eq!(restore(&[]).dir, PathBuf::from(DEFAULT_DIR));
    }

    #[test]
    fn a_bare_restore_is_a_plan_and_nothing_else() {
        // 决策 150：默认形态**不是** `--apply`，也不是"两者都不是的未定义状态"。
        let args = restore(&[]);
        assert!(!args.apply, "不带开关时绝不能真的动手");
        assert!(!args.dry_run, "默认形态不该伪装成显式的 --dry-run");
        assert!(!args.with_fix, "决策 151：fix 默认不选中");
        assert!(!args.json);
        assert!(args.only.is_empty(), "空 --only 表示四个 section 都要");
    }

    #[test]
    fn only_takes_the_four_slugs_and_repeats() {
        let args = restore(&["--only", "path", "--only", "env"]);
        assert_eq!(args.only, vec![SectionId::Path, SectionId::Env]);
    }

    #[test]
    fn only_is_generated_from_the_section_table() {
        // 四个 slug 一个不多一个不少 —— 表在 core，这里只是把它走一遍。
        for id in SectionId::ALL {
            let args = restore(&["--only", id.as_str()]);
            assert_eq!(args.only, vec![id], "{} 应当能被 --only 接受", id.as_str());
        }
    }

    #[test]
    fn an_unknown_section_is_a_usage_error() {
        let error = Cli::try_parse_from(["tuoen", "restore", "--only", "toolsx"])
            .expect_err("不认识的 section 必须在解析期被拒");
        assert_eq!(error.exit_code(), 2, "用法错误就是退出码 2");
        let text = error.to_string();
        assert!(text.contains("toolsx"), "错误里要点名那个取值：{text}");
        assert!(text.contains("path"), "错误里要列出合法取值：{text}");
    }

    #[test]
    fn apply_and_dry_run_cannot_be_given_together() {
        let error = Cli::try_parse_from(["tuoen", "restore", "--apply", "--dry-run"])
            .expect_err("--apply 与 --dry-run 是矛盾的，必须在解析期拒绝");
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn dry_run_alone_is_accepted_because_it_is_the_default_spelled_out() {
        let args = restore(&["--dry-run"]);
        assert!(args.dry_run);
        assert!(!args.apply);
    }
}
