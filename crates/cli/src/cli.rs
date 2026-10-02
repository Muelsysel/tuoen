//! 命令行参数定义。
//!
//! **中文优先**：`about` / `help` 文本是中文。但**子命令名、参数名、`--json` 的键与取值
//! 一律英文且不本地化** —— 否则脚本与未来的 GUI 会被界面语言绑死（决策 35）。

use clap::{Args, Parser, Subcommand};

use crate::catalog::CatalogCommand;
use crate::detect::DetectArgs;
use crate::manage::{InstallArgs, UninstallArgs, UseArgs};
use crate::path::PathCommand;
use crate::shim::ShimCommand;

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

    /// 下载并安装一个工具的某个版本到 tuoen 的存储里。
    #[command(long_about = r#"下载并安装一个工具的某个版本到 tuoen 的存储里。

**装完不等于生效。** 这个命令只把版本放进存储，**当前生效的版本一动不动** ——
要生效请再敲一次 `tuoen use`，或者这次就加 `--use`。

为什么刻意不顺手激活：本项目的招牌是**多版本共存**。
顺手激活意味着"装一个旧版本去做兼容性排查"会**静默改变正在生效的环境**，
而已经开着的终端、IDE、构建脚本都不会知道
（Windows 的环境块是进程创建时复制的，改不进去已经跑着的进程）。

哈希**来自内置目录，不来自下载源**：从你正在下载的那台服务器上取哈希
等于没有校验 —— 能改制品的人也能改哈希。

用法：
  tuoen install node              装最新可安装的版本
  tuoen install node@24.19.0      装指定版本
  tuoen install node@24 --use     装完立刻激活
  tuoen install node --dry-run    只说会做什么，什么都不下载"#)]
    Install(InstallArgs),

    /// 让某个**已经装过**的版本生效（原子翻转 `current`）。
    #[command(long_about = r#"让某个已经装过的版本生效。

实现方式是把存储里的 `current` 联接**就地重指**到那个版本目录 ——
一次 IOCTL，**没有"先删再建"的空窗**，所以任何时刻 `current` 都是可解析的
（`docs/DESIGN.md` 决策 46）。

**这个命令不改 `PATH`，也不改任何 shim。** 它只翻转一个链接；
`PATH` 上要放什么、shim 怎么发，是另外两件事。"#)]
    Use(UseArgs),

    /// 从 tuoen 的存储里删掉一个版本。
    #[command(long_about = r#"从 tuoen 的存储里删掉一个版本。

删的正好是当前激活版本时，**默认拒绝执行**，并告诉你怎么做：
先 `tuoen use` 到别的版本，或者加 `--force`（会先摘掉 `current` 再删）。

拒绝是因为"`current` 指向一个已经没了的目录"会让之后每一个 shim
都报一个与真实原因无关的错误 —— 那种错误最难查。

这个命令**只删 tuoen 自己装的东西**。别人装的（winget / Scoop / nvm…）
不在存储里，也不归我们管。"#)]
    Uninstall(UninstallArgs),

    /// 把工具的命令以真 `.exe` 的形式放到 `PATH` 上（shim）。
    #[command(
        subcommand,
        long_about = r#"把工具的命令以**真 `.exe`** 的形式放到 `PATH` 上。

为什么必须是自己发 `.exe`：进程的 `PATH` 是「机器条目在前、用户条目在后」，
所以用户级工具**永远输掉名字冲突**，只能靠一个真的可执行文件抢回名字（决策 10）；
而 `.cmd` / `.ps1` 在 Node ≥18.20.2 之后**无法被 spawn**（CVE-2024-27980），
参数还要再过一遍 cmd 的解析器。

shim 把目标路径**烘进二进制**，运行时一个文件都不读。它指向
`<存储>/<工具>/current`（一个链接），**不是**某个具体版本目录 ——
所以切版本（`tuoen use`）**不必重新生成 shim**（决策 11）。

shim 目录与存储**并列**（`<家目录>/shims`），不在 `store/` 里面：
`store/` 可以被清空重来，而 `PATH` 上那批文件是用户环境的一部分。

**这一族不改 `PATH`。** 它只把文件放进 shim 目录；要让它生效，
把这个目录加进 `PATH`（`tuoen shim path` 给你它的绝对路径）。"#
    )]
    Shim(ShimCommand),

    /// 看清并修改 `PATH`（`show` 只读，`add` / `remove` 先出计划再落盘）。
    #[command(
        subcommand,
        long_about = r#"看清并修改 `PATH`。

**不带子命令时等于 `tuoen path show`** —— 看是最安全的默认动作，而"敲了
`tuoen path` 就改了 `PATH`"是最不该发生的默认动作。

三个子命令：
  show          只读地报告现状（长度预算、重复、失效条目、遮蔽……）
  add <dir>     把目录追加到**用户级** `PATH`
  remove <dir>  从**用户级** `PATH` 里删掉目录

## 为什么这件事必须这么小心

`PATH` 是本项目里**唯一**一处「一次调用就能让整台机器所有命令失效」的地方：

* `setx` 在 **1024** 字符处**静默裁剪**，并把你值里所有 `%VAR%` **永久展开**成
  字面量 —— 所以 **tuoen 绝不调用 `setx`**。写回走的是一次整条值写完
  （不是逐段改：逐段改会有"`PATH` 暂时少了几个目录"的中间状态），
  然后广播 `WM_SETTINGCHANGE`。
* `cmd.exe` 在 `PATH` 超过 **8191** 字符后**完全忽略整条 `PATH`** —— 不是部分失效，
  是所有命令一起失效。这两个数字是**不同的悬崖**，混为一谈会让告警出现在错误的位置上。

## 四件平台事实

1. 进程 `PATH` = **机器条目在前、用户条目在后**，所以用户级工具永远输掉名字冲突
   —— 这就是 shim 存在的理由（决策 10）。
2. 机器级 `PATH` 要**提权**才能写，而 tuoen **不做"顺手提权"**：`add` / `remove`
   只动用户级，机器级只读。
3. 进程 `PATH` 里可能有**注册表里没有的条目**（进程注入，本机实测有 PowerShell 的
   MSIX 别名）。`show` 单独把它们列出来，因为只看注册表会漏掉它们，而它们真的占长度。
4. **已经跑着的终端 / IDE 拿不到新环境**（环境块是 `CreateProcess` 时复制的）——
   写完要新开一个终端。这是平台限制，不是命令没生效。

## 只报告，不自动清理

重复条目、指向不存在目录的条目、硬编码了用户名的条目、被别的目录抢在前面的 shim
—— 这些 `show` **全部只报告**（决策 24）。`PATH` 上的东西是用户的。

## `--dry-run`

`add` / `remove` 都有 `--dry-run`，它走的是与真写**同一套**计划代码，只是不落盘。"#
    )]
    Path(PathCommand),
}

/// `tuoen path` 的参数。
///
/// # 为什么这里没有第二层
///
/// clap 的 `#[derive(Subcommand)]` **只支持枚举**，而"带 `#[command(subcommand)]`
/// 的结构体"必须自己是一个枚举 —— 于是 `PathArgs`（结构体）会被当成叶子命令，
/// 它里面的子命令**根本不会被注册**（症状是 `tuoen path show` 报
/// `unrecognized subcommand`）。所以这一族只有**一层**枚举：
/// [`crate::path::PathCommand`] 直接挂在 [`Command::Path`] 上。
///
/// # 为什么 `command` 不是 `Option`
///
/// clap 4 没有 `default_subcommand`，而"不带子命令时做什么"是**本族的核心契约**
/// （`tuoen path` ≡ `tuoen path show`）。它由 [`crate::path::default_path_subcommand`]
/// 在**进入 clap 之前**补上 —— 于是 `tuoen path` 与 `tuoen path show` 在 clap 眼里
/// 是**同一条命令行**，不可能走岔。见那个函数的文档。
///
/// # `--json` 在哪儿
///
/// 在**每一个叶子命令**上（`tuoen path show --json` / `add … --json` /
/// `remove … --json`），与 `shim` 那一族一致。父级上没有 `--json`
/// （`tuoen path --json` 是用法的错），因为不带子命令时它已经被补成 `show`，
/// 而"`--json` 属于哪一个命令"只有一个答案比两个答案更难写错。

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

    #[test]
    fn the_manage_family_is_top_level() {
        // `install` / `use` / `uninstall` 动磁盘，`catalog` 只读目录 ——
        // 把动磁盘的命令藏进只读的那一族会让"看一眼"和"改一台机器"看起来一样危险。
        assert!(matches!(
            Cli::try_parse_from(["tuoen", "install", "node"])
                .expect("parse")
                .command,
            Command::Install(_)
        ));
        assert!(Cli::try_parse_from(["tuoen", "catalog", "install", "node"]).is_err());
    }

    #[test]
    fn the_shim_family_is_top_level() {
        // shim 会**往 PATH 上放可执行文件**（以及删掉它们）—— 与 `catalog` 那种
        // 只读目录的命令不是一类，藏进 catalog 会让"看一眼"和"改 PATH"看起来一样。
        assert!(matches!(
            Cli::try_parse_from(["tuoen", "shim", "list"])
                .expect("parse")
                .command,
            Command::Shim(_)
        ));
        assert!(Cli::try_parse_from(["tuoen", "catalog", "shim", "list"]).is_err());
    }
}
