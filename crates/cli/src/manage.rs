//! `tuoen install` / `tuoen use` / `tuoen uninstall` —— 真正动磁盘的那一族。
//!
//! **这三个命令是 L0 的出口。** 前面的票建立的是"知道什么能装、怎么安全地解压、
//! 怎么原子地翻转一个 junction"；这一族把它们串成用户真的会敲的东西。
//!
//! ## 一条刻意的分工：`install` **不**自动激活
//!
//! 装完不等于生效。`tuoen install node@24.19.0` 把版本放进存储，
//! **`current` 一动不动**；要生效得显式 `tuoen use node 24.19.0`（或 `install --use`）。
//!
//! 为什么不学 `nvm install` 那样顺手激活：
//!
//! - 本项目的招牌是**多版本共存**与"我能控制现在生效的是哪一个"。
//!   顺手激活意味着"装一个旧版本去做兼容性排查"会**静默改变正在生效的环境** ——
//!   而正在运行的终端、IDE、构建脚本不会知道这件事（环境块是 `CreateProcess`
//!   时复制的，见 `docs/DESIGN.md` §2）。
//! - 激活是**一次原子操作**（决策 46），它能被单独观察、单独测试、单独回滚。
//!   把它藏在 `install` 里会让"到底哪一步改了 `current`"变得需要读源码才知道。
//!
//! 代价是第一次用的人会问"我装完了怎么敲不到"。所以安装成功后的输出
//! **必须把下一步命令原样打出来** —— 这不是友好，是接口完整性。
//!
//! ## 参数形状
//!
//! `install` 收 `node@24.19.0` 或 `node`（后者取目录里最新的可安装版本）；
//! `use` 与 `uninstall` 收两个位置参数。**不一致是刻意的**：`install` 的
//! `@` 语法与 `npm`/`mise` 一致（用户带着这个直觉来），而 `use` 的两个参数
//! 没有歧义空间，加 `@` 只会多一种写法。

use clap::Args;

/// `tuoen install <tool>[@<version>]`
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// 工具 id 或别名，可带版本：`node` / `node@24.19.0` / `node@24`。
    ///
    /// 不带版本时取**目录里最新的可安装版本**（被许可证门禁拒绝的不算）。
    pub spec: String,

    /// 目标平台。默认 `windows-x64`。
    #[arg(long, default_value = "windows-x64")]
    pub platform: String,

    /// 装完之后立刻激活（等价于紧接着跑一次 `tuoen use`）。
    #[arg(long)]
    pub r#use: bool,

    /// 只说明会做什么，**什么都不下载、什么都不写**。
    #[arg(long)]
    pub dry_run: bool,

    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}

/// `tuoen use <tool> <version>`
#[derive(Debug, Args)]
pub struct UseArgs {
    /// 工具 id 或别名。
    pub tool: String,

    /// 要激活的版本。**必须已经装过**（`tuoen list` 能看到）。
    pub version: String,

    /// 输出稳定的 JSON。
    #[arg(long)]
    pub json: bool,
}

/// `tuoen uninstall <tool> <version>`
#[derive(Debug, Args)]
pub struct UninstallArgs {
    /// 工具 id 或别名。
    pub tool: String,

    /// 要删掉的版本。
    pub version: String,

    /// 删的正好是当前激活版本时，**先摘掉 `current` 再删**。
    ///
    /// 不加这个开关时命令会拒绝执行并告诉你先 `tuoen use` 到别的版本 ——
    /// 因为"把 `current` 指向一个已经没了的目录"会让之后每一个 shim
    /// 都报一个与真实原因无关的错误。
    #[arg(long)]
    pub force: bool,

    /// 输出稳定的 JSON。
    #[arg(long)]
    pub json: bool,
}

/// 把 `install` 的位置参数切成 `(工具, 可选的版本约束)`。
///
/// 只切**第一个** `@`：版本串里可以有 `+`（Temurin 的 `21.0.12.1+1`）
/// 也可以有 `.`，但**不会有 `@`**，而工具 id 是我们自己的（小写无空格）。
///
/// 写成自由函数而不是 `clap` 的 `value_parser`：这样"`node@` 是错的"
/// 这类判断留在我们手里，错误消息也是中文的。
#[must_use]
pub fn split_spec(spec: &str) -> (String, Option<String>) {
    match spec.split_once('@') {
        Some((tool, version)) => {
            let tool = tool.trim().to_owned();
            let version = version.trim();
            if version.is_empty() {
                // `node@` 与 `node@   ` 都是"你写了 @ 但没写版本"。
                (tool, Some(String::new()))
            } else {
                (tool, Some(version.to_owned()))
            }
        }
        None => (spec.trim().to_owned(), None),
    }
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{InstallArgs, split_spec};
    use crate::cli::{Cli, Command};

    fn install(spec: &str) -> InstallArgs {
        match Cli::try_parse_from(["tuoen", "install", spec])
            .expect("parse")
            .command
        {
            Command::Install(args) => args,
            other => panic!("应当是 install，实际：{other:?}"),
        }
    }

    #[test]
    fn install_takes_a_bare_tool() {
        let args = install("node");
        assert_eq!(args.spec, "node");
        assert_eq!(args.platform, "windows-x64");
        assert!(!args.r#use);
        assert!(!args.dry_run);
        assert!(!args.json);
    }

    #[test]
    fn install_requires_a_spec() {
        assert!(Cli::try_parse_from(["tuoen", "install"]).is_err());
    }

    #[test]
    fn the_at_split_takes_only_the_first_at() {
        // 版本串里可以有 `+` 与 `.`，但不会有 `@`；工具 id 也不会有。
        assert_eq!(
            split_spec("node@24.19.0"),
            ("node".to_owned(), Some("24.19.0".to_owned()))
        );
        assert_eq!(
            split_spec("temurin@21.0.12.1+1"),
            ("temurin".to_owned(), Some("21.0.12.1+1".to_owned()))
        );
        assert_eq!(split_spec("node"), ("node".to_owned(), None));
    }

    #[test]
    fn an_empty_version_after_at_is_not_the_same_as_no_version() {
        // `node@` 是"你写了 @ 但没写版本"，要报错；`node` 是"给我最新的"。
        // 把两者混成一种会让 `tuoen install node@` 静默装一个最新版。
        assert_eq!(
            split_spec("node@"),
            ("node".to_owned(), Some(String::new()))
        );
        assert_eq!(
            split_spec("node@   "),
            ("node".to_owned(), Some(String::new()))
        );
        assert_ne!(split_spec("node@"), split_spec("node"));
    }

    #[test]
    fn the_at_split_trims_both_sides() {
        assert_eq!(
            split_spec("  node @ 24.19.0 "),
            ("node".to_owned(), Some("24.19.0".to_owned()))
        );
    }

    #[test]
    fn use_needs_both_positionals() {
        assert!(Cli::try_parse_from(["tuoen", "use", "node"]).is_err());
        match Cli::try_parse_from(["tuoen", "use", "node", "24.19.0"])
            .expect("parse")
            .command
        {
            Command::Use(args) => {
                assert_eq!(args.tool, "node");
                assert_eq!(args.version, "24.19.0");
            }
            other => panic!("应当是 use，实际：{other:?}"),
        }
    }

    #[test]
    fn uninstall_force_defaults_to_off() {
        // 默认必须是"拒绝删当前版本"—— 危险的默认值不该靠用户记得加开关。
        match Cli::try_parse_from(["tuoen", "uninstall", "node", "24.19.0"])
            .expect("parse")
            .command
        {
            Command::Uninstall(args) => assert!(!args.force),
            other => panic!("应当是 uninstall，实际：{other:?}"),
        }
        match Cli::try_parse_from(["tuoen", "uninstall", "node", "24.19.0", "--force"])
            .expect("parse")
            .command
        {
            Command::Uninstall(args) => assert!(args.force),
            other => panic!("应当是 uninstall，实际：{other:?}"),
        }
    }

    #[test]
    fn cli_definition_is_still_valid_after_adding_the_manage_family() {
        Cli::command().debug_assert();
    }
}
