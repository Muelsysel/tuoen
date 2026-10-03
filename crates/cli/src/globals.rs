//! `tuoen globals` —— 全局包那一族（ticket #23）。
//!
//! 现在只有 `list` 一条子命令。`add` / `remove` 是后面的票 —— 那时这条命令会
//! 第一次**往我们的根里装东西**，所以这一族的骨架现在就要说清楚"读"与"写"的分界。
//!
//! # 为什么 `tuoen globals` 不带子命令是用法错误
//!
//! 与 `path` 那一族刻意不同（那里 `tuoen path` ≡ `tuoen path show`）：`globals`
//! 的未来里有两个**会改磁盘**的动作（装、删）。把"什么都不带"绑到"列出"上，
//! 是在给一条以后要长大的命令定一个**默认动作** —— 而默认动作的代价在写入侧。
//! 不带子命令时 clap 打用法（退出码 2），与 `shim` 那一族一致。
//!
//! # `--json` 在哪儿
//!
//! 在叶子上（`tuoen globals list --json`），与其它族一致：`tuoen globals --json`
//! 是用法的错。父级上没有 `--json` 就没有"这个开关属于谁"的歧义。

use clap::{Args, Subcommand};

/// `tuoen globals` 的子命令。
#[derive(Debug, Subcommand)]
pub enum GlobalsCommand {
    /// 列出两个来源的全局包（工具自己的 + tuoen 管的）。
    #[command(long_about = r#"列出**两个来源**的全局包，只读。

  machine  工具自己说它的全局位置在哪、里面有什么（`npm ls -g` / `pip list`）
  tuoen    `%LOCALAPPDATA%\tuoen\globals\` 里由 tuoen 管的那些

**为什么必须两个都列**：tuoen 装包时只改**子进程环境**（`NPM_CONFIG_PREFIX` /
`PYTHONUSERBASE`），一个字节都不写进 `.npmrc` / `pip.ini` / 注册表（决策 27）。
所以用户自己的终端里的 `npm ls -g` 说的是真话，而它**看不到** tuoen 管的那些。
只报一个来源就会撒谎：`restore` 之后的"再捕获一次"会得到一份少了所有 tuoen 包的清单。

同一个工具的两个来源是**两行**（`globals.toml` 里多一个 `source` 字段，决策 166
的加法变更）。包名可能在两边都出现 —— 那不是重复，是**两套安装**。

## `roots`

`--json` 的 `roots` 里连**空的根**都会出，带 `exists` 之外的两个字段
（`tool` / `root` / `source`）。"根在哪里、它是空的"必须说得出来：
只列"有包的根"会让"tuoen 一个包都没管"与"我们没看"长得一模一样。

## `binNames`

每个包带一个 `binNames`：它提供了哪些命令。npm 读包的 `package.json` 里的
`bin` 字段（对象 → 键；字符串 → 包名），pip 读 `PYTHONUSERBASE` 下
`Scripts\*.exe` 的文件名。**拿不到时整键消失**（不是空数组）：
"我没查出来"与"它一个命令都没有"是两件事。

## 退出码

  0  列出来了（**包括两个来源都空**）
  1  算不出根（进程环境里没有 `LOCALAPPDATA`）
  2  用法错误"#)]
    List(GlobalsListArgs),
}

/// `tuoen globals list` 的参数。
#[derive(Debug, Args)]
pub struct GlobalsListArgs {
    /// 输出稳定的 JSON（键与取值不本地化）。
    #[arg(long)]
    pub json: bool,
}
