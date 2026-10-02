//! `tuoen path diff` / `tuoen path apply` 的参数。
//!
//! **长帮助不在参数结构体里**：那三条必须说清的事（diff 是报告 / apply 要用户把意图
//! 说出来 / `--dry-run` 与真写共用同一套计划）写在 [`crate::cli::Command::Path`] 上，
//! 与 `capture` / `detect` 的位置一致 —— 帮助文本只有一处。
//!
//! # 为什么 `--only` 是**解析过的类**而不是字符串
//!
//! 一个拼错的类（`--only adds`）必须被 clap 以退出码 2 **当场**拒掉。若它被静默忽略，
//! 用户拿到的是"退出 0 + 什么都没做"，而他以为改过了 —— 这正是这一票最不能出现的假话
//! （与 `capture --only tool` 同一条理由）。
//!
//! 取值是 [`DiffClass::parse`] 的**精确匹配**（大小写敏感）：大小写在这里是数据，
//! 不是风格 —— `case-only` 与 `caseonly` 不该都能过。
//!
//! # 为什么 `--pick` 收字符串而不是下标
//!
//! 取值就是 `path diff` 输出里的 `id` **逐字**（`user:12` / `user:+16`）。
//! 下标在用户读到输出、敲下命令之间会因为环境变化而失效；`id` 至少能让人看出
//! "这条已经不在了"（决策 129 的同一条理由）。`+` 那半边是必需的：同一下标既可能
//! 有一条 `remove`、又可能有一条 `add`，两种都用 `user:0` 会让一次 `--pick` 选中两条
//! （想删一条，结果还顺手加了一条 —— 决策 143）。

use std::path::PathBuf;

use clap::Args;
use tuoen_core::pathdiff::DiffClass;

/// 不给 `<snapshot>` 时用的快照路径。
///
/// 与 `capture` 的默认输出目录（`tuoen.d`）一致：`tuoen capture --only path` 之后
/// 直接敲 `tuoen path diff` 就能看到结果，中间不需要记一个路径。
pub const DEFAULT_SNAPSHOT: &str = "tuoen.d/path.toml";

/// `tuoen path diff` 的参数。
#[derive(Debug, Args)]
pub struct PathDiffArgs {
    /// 目标快照（`tuoen.d/path.toml`）。**本机侧**是现场采集的，不走文件。
    #[arg(value_name = "SNAPSHOT", default_value = DEFAULT_SNAPSHOT)]
    pub snapshot: PathBuf,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

/// `tuoen path apply` 的参数。
#[derive(Debug, Args)]
pub struct PathApplyArgs {
    /// 目标快照（`tuoen.d/path.toml`）。
    #[arg(value_name = "SNAPSHOT", default_value = DEFAULT_SNAPSHOT)]
    pub snapshot: PathBuf,

    /// 按类选（可重复）。取值就是六个 slug：keep / add / remove / move / fix / case-only。
    ///
    /// **不给 `--only` 也不给 `--pick` = 什么都没选**，`path apply` 会拒绝执行
    /// （`nothing-selected`，退出码 1）：默认值一旦存在，"顺手删掉几条"就会成为
    /// 默认行为，而这一票动的是用户整条 `PATH`。
    #[arg(long = "only", value_name = "CLASS", value_parser = parse_class)]
    pub only: Vec<DiffClass>,

    /// 按 id 选（可重复）。取值是 `path diff` 输出里的 `id` 逐字（如 `user:12` / `user:+16`）。
    #[arg(long = "pick", value_name = "ID")]
    pub pick: Vec<String>,

    /// 只报计划，**什么都不写**。
    ///
    /// 它走的是与真写**同一套** `diff` + `rebuild` + 计划构造，区别只在最后不落盘
    /// （决策 139）。漂移的预览比没有预览更危险：用户会照着预览的结论去点"确认"。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    ///
    /// 与 `tuoen shell` 不同，这里**不需要**配 `--dry-run`：这条命令不 spawn 子进程，
    /// stdout 上没有第二个写者（决策 139）。
    #[arg(long)]
    pub json: bool,
}

/// `--only` 的取值解析。**唯一**一处把字符串变成 [`DiffClass`] 的地方。
///
/// 取值集合由 core 的 [`DiffClass::ALL`] 决定，这里**不抄一张表** ——
/// 抄一份的那天起，core 加了一类而 CLI 的帮助里没有它。
fn parse_class(text: &str) -> Result<DiffClass, String> {
    DiffClass::parse(text).ok_or_else(|| {
        let known = DiffClass::ALL
            .iter()
            .map(|class| class.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        format!("`{text}` 不是一个类；取值是 {known}")
    })
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::{DEFAULT_SNAPSHOT, PathApplyArgs, PathDiffArgs};
    use crate::cli::{Cli, Command};
    use crate::path::PathCommand;

    /// 解析一次 `tuoen path …` 并取出 `path` 那一族的命令。
    ///
    /// 走的是与生产代码同一套参数补齐（`default_path_subcommand`），所以这里测到的
    /// 就是用户真的敲下去之后会发生的事。
    fn path_command(args: &[&str]) -> PathCommand {
        let mut full = vec![
            std::ffi::OsString::from("tuoen"),
            std::ffi::OsString::from("path"),
        ];
        full.extend(args.iter().map(std::ffi::OsString::from));
        match Cli::try_parse_from(crate::path::default_path_subcommand(full))
            .expect("parse")
            .command
        {
            Command::Path(command) => command,
            other => panic!("应当是 path，实际：{other:?}"),
        }
    }

    #[test]
    fn diff_defaults_to_the_snapshot_capture_writes() {
        match path_command(&["diff"]) {
            PathCommand::Diff(PathDiffArgs { snapshot, json }) => {
                assert_eq!(snapshot, PathBuf::from(DEFAULT_SNAPSHOT));
                assert!(!json);
            }
            other => panic!("应当是 path diff，实际：{other:?}"),
        }
    }

    #[test]
    fn apply_defaults_to_no_selection_at_all() {
        // **这一族最重要的一条默认值**：不给选择就是什么都没选，而不是"全选"。
        // 默认值一旦存在，"顺手删掉几条"就会成为默认行为。
        match path_command(&["apply"]) {
            PathCommand::Apply(PathApplyArgs {
                snapshot,
                only,
                pick,
                dry_run,
                json,
            }) => {
                assert_eq!(snapshot, PathBuf::from(DEFAULT_SNAPSHOT));
                assert!(only.is_empty());
                assert!(pick.is_empty());
                assert!(!dry_run, "默认必须是真写 —— 但真写需要先有选择");
                assert!(!json);
            }
            other => panic!("应当是 path apply，实际：{other:?}"),
        }
    }

    #[test]
    fn only_is_repeatable_and_keeps_the_order_the_user_gave() {
        match path_command(&["apply", "--only", "add", "--only", "fix", "--dry-run"]) {
            PathCommand::Apply(args) => {
                assert_eq!(
                    args.only,
                    vec![
                        tuoen_core::pathdiff::DiffClass::Add,
                        tuoen_core::pathdiff::DiffClass::Fix
                    ]
                );
                assert!(args.dry_run);
            }
            other => panic!("应当是 path apply，实际：{other:?}"),
        }
    }

    #[test]
    fn an_unknown_class_is_a_usage_error_not_a_silent_noop() {
        // 退出码 2 —— 静默忽略一个拼错的类会让"我以为改过了"变成一句假话。
        let error = Cli::try_parse_from(["tuoen", "path", "apply", "--only", "adds"])
            .expect_err("`adds` 不是 `add`，必须被拒");
        assert_eq!(error.exit_code(), 2, "{error}");
        // 大小写敏感：`case-only` 与 `caseonly` 不该都能过。
        assert!(
            Cli::try_parse_from(["tuoen", "path", "apply", "--only", "Case-Only"]).is_err(),
            "大小写在这里是数据，不是风格"
        );
    }

    #[test]
    fn pick_keeps_both_id_shapes_verbatim() {
        match path_command(&[
            "apply",
            "--pick",
            "user:12",
            "--pick",
            "user:+16",
            "--dry-run",
        ]) {
            PathCommand::Apply(args) => {
                assert_eq!(args.pick, vec!["user:12".to_owned(), "user:+16".to_owned()]);
            }
            other => panic!("应当是 path apply，实际：{other:?}"),
        }
    }

    #[test]
    fn the_usage_error_lists_every_class() {
        // 错误消息要说清取值有哪些 —— 判据是 core 的 `ALL`，不是这里的一张表。
        let message = super::parse_class("nope").expect_err("必须被拒");
        for class in tuoen_core::pathdiff::DiffClass::ALL {
            assert!(message.contains(class.as_str()), "{message}");
        }
    }
}
