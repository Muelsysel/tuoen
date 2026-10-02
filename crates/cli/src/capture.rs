//! `tuoen capture` 的参数。
//!
//! **长帮助不在这里**：那三条必须说清的事（这条命令只读 / 这份 `tuoen.d/` 是给人
//! 提交进仓库的 / `--only` 是"格式层面"的选择性捕获）写在
//! [`crate::cli::Command::Capture`] 上，与 `detect` / `list` / `catalog` 的位置一致 ——
//! 参数结构体里只留形状与取值，帮助文本只有一处。
//!
//! # 为什么 `--only` 是枚举而不是字符串
//!
//! 一个拼错的 section（`--only tool`）必须被 clap 以退出码 2 **当场**拒掉。
//! 若它被静默忽略，用户拿到的是一份**少了 tools 的快照**和一句"成功" ——
//! 而"我以为捕获了"正是这一票最不能出现的假话（`docs/DESIGN.md` 决策 35 的同类）。

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use tuoen_core::capture::Section;

/// `tuoen capture` 的参数。
#[derive(Debug, Args)]
pub struct CaptureArgs {
    /// 只捕获这些 section（可重复）。不给就是全部四个：tools / path / env / wsl。
    #[arg(long = "only", value_name = "section")]
    pub only: Vec<CaptureSection>,

    /// 写到哪个目录（**相对当前目录**）。
    #[arg(long, value_name = "dir", default_value = "tuoen.d")]
    pub out: PathBuf,

    /// 不探测版本（快得多；`tools.toml` 里的版本会是空的）。
    #[arg(long)]
    pub no_version: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

/// `--only` 的取值。**取值是稳定 slug，不本地化。**
///
/// 它与 [`Section`] 一一对应，但**不是**同一个类型：CLI 的取值集合是对外契约
/// （改了要递增信封的 `schemaVersion`），引擎的 [`Section`] 是内部枚举。
/// 两者的映射只有 [`Self::section`] 这一处，所以它们不可能漂移。
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CaptureSection {
    /// 工具与版本（`tools.toml`）。
    Tools,
    /// `PATH` 的结构（`path.toml`）。
    Path,
    /// 持久环境变量（`env.toml`）。
    Env,
    /// WSL 发行版（`wsl.toml`）。
    Wsl,
}

impl CaptureSection {
    /// 映射到引擎的 section。**全仓唯一一处映射。**
    #[must_use]
    pub const fn section(self) -> Section {
        match self {
            Self::Tools => Section::Tools,
            Self::Path => Section::Path,
            Self::Env => Section::Env,
            Self::Wsl => Section::Wsl,
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::CaptureSection;
    use crate::cli::{Cli, Command};

    #[test]
    fn a_bare_capture_takes_every_section_and_the_default_out_dir() {
        let cli = Cli::try_parse_from(["tuoen", "capture"]).expect("parse");
        match cli.command {
            Command::Capture(args) => {
                assert!(args.only.is_empty(), "不给 `--only` 就是全部");
                assert_eq!(args.out, std::path::PathBuf::from("tuoen.d"));
                assert!(!args.no_version, "默认要探测版本");
                assert!(!args.json);
            }
            other => panic!("应当是 capture，实际：{other:?}"),
        }
    }

    #[test]
    fn only_is_repeatable_and_keeps_the_order_the_user_gave() {
        // **顺序在 CLI 这一层保留**：排序是 `CaptureOptions::effective_sections` 的事
        // （它要去重并排序，因为文件的字节顺序是承诺）。在这里顺手排序会让
        // "用户给了什么"这个事实消失，而它是 `--json` 之前唯一能看到输入的地方。
        let cli = Cli::try_parse_from(["tuoen", "capture", "--only", "env", "--only", "path"])
            .expect("parse");
        match cli.command {
            Command::Capture(args) => {
                assert_eq!(args.only, vec![CaptureSection::Env, CaptureSection::Path])
            }
            other => panic!("应当是 capture，实际：{other:?}"),
        }
    }

    #[test]
    fn an_unknown_section_is_a_usage_error_not_a_silent_noop() {
        // clap 的用法错误必须是退出码 2；静默忽略一个拼错的 section 会让
        // "我以为捕获了 tools"变成一句假话。
        let error = Cli::try_parse_from(["tuoen", "capture", "--only", "tool"])
            .expect_err("`tool` 不是 `tools`，必须被拒");
        assert_eq!(error.exit_code(), 2, "{error}");
    }

    #[test]
    fn every_cli_section_maps_onto_an_engine_section() {
        for section in [
            CaptureSection::Tools,
            CaptureSection::Path,
            CaptureSection::Env,
            CaptureSection::Wsl,
        ] {
            // 文件名与 slug 都由引擎定义，CLI 不重新拼 —— 于是"`--only` 的取值"
            // 与"磁盘上的文件名"不可能对不上。
            assert!(
                section.section().file_name().ends_with(".toml"),
                "{section:?} 没有对应的文件"
            );
        }
    }
}
