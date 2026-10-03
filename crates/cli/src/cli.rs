//! 命令行参数定义。
//!
//! **中文优先**：`about` / `help` 文本是中文。但**子命令名、参数名、`--json` 的键与取值
//! 一律英文且不本地化** —— 否则脚本与未来的 GUI 会被界面语言绑死（决策 35）。

use clap::{Args, Parser, Subcommand};

use crate::capture::CaptureArgs;
use crate::catalog::CatalogCommand;
use crate::detect::DetectArgs;
use crate::doctor::DoctorArgs;
use crate::lock_cmd::LockArgs;
use crate::manage::{InstallArgs, UninstallArgs, UseArgs};
use crate::path::PathCommand;
use crate::restore::RestoreArgs;
use crate::shell::{AutoArgs, ShellArgs};
use crate::shim::ShimCommand;
use crate::trust_cmd::TrustArgs;

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

    /// 只读地把整机开发状态写成一个可提交进仓库的 `tuoen.d/`。
    #[command(long_about = r#"只读地把整机开发状态写成一个 `tuoen.d/` 目录。

**这条命令只读。** 它不写 `PATH`、不写注册表、不动任何工具 ——
它把读到的六样东西写成文件。真正改机器的那条路是 `restore`。

## 写出来的文件是给人提交进仓库的

  schema.toml    这一份快照是谁写的、包含哪些 section
  tools.toml     工具与版本（每条带来源与置信度）
  path.toml      `PATH` 的结构（作用域、顺序、长度预算、reparse 形状）
  env.toml       持久环境变量（用户级 + 机器级）
  wsl.toml       WSL 发行版与它们**实际**的 vhdx 路径
  globals.toml   全局包清单（`npm -g` / `pip`）——**只捕获，L1 不还原**
  configs.toml   配置文件清单（路径 + 哈希）——**只捕获，L1 不还原**
  skipped.toml   看见了但**故意没写**的东西，以及为什么

正因为它要进仓库，`skipped.toml` 是这份快照的一部分而不是一条日志：
形似凭据的环境变量**不会被写进 `env.toml`**，而是记进跳过清单（只有名字与原因，
没有任何材料）。**静默跳过是 bug** —— 它会让「都备份好了」变成一句假话。
这份目录是可 diff 的：换机器时，两份 `tuoen.d/` 的差异就是你要重建的东西。

## `globals` / `configs` 在 L1 只捕获、不还原

它们是 dev-state 的最后两块拼图，而**还原**它们各有各的理由不做：全局包要重装就得跑
包管理器自己的解析（网络 + 权限 + 版本约束），配置文件更是**不能替用户写** ——
里面可能有凭据、可能有只在这一台机器上成立的绝对路径。

所以 `restore` 遇到带了这两个 section 的快照时会**明说**「我看见了，但我不还原它们」，
而不是让一份四条全 `no-change` 的计划看起来像「这份快照里的东西本机都有了」。
`--json` 里那句话是 `summary.unrestorable`（只在真的有它们时才出这个键）。

## 配置文件清单里**只有路径与哈希，没有内容**

`configs.toml` 不存任何配置文件的正文：存正文等于把配置文件（可能含凭据）复制进仓库。
判据是「内容级」的 —— 形似凭据的文件会被跳过并记进 `skipped.toml`，理由具体到 slug。
本机的 `.m2/settings.xml` 里就有一个明文 PAT，它正是这一条存在的理由。

## `--only` 是「格式层面」的选择性捕获，不是事后过滤

只捕获 `path` 时，其余几个 `.toml` **根本不会被生成**，而 `schema.toml` 的 `sections`
里只有 `["path"]` —— 于是「这份快照没捕获 tools」与「这台机器上没有工具」是**两件分得开
的事**。事后过滤做不到这一点：它读得出「没有」，读不出「没看」，而 `restore` 会把
「没看」读成「没有」。`skipped.toml` 也是这个道理：**没扫过环境变量、也没扫过配置文件**
就没有这个文件，因为它要说的是「扫过了，跳过了什么」。

## 默认值

写到**当前目录**下的 `tuoen.d/`。`--no-version` 跳过版本探测（快得多：`tools.toml`
里的版本会是空的，`globals.toml` 的 `tool_version` 会是 `unknown` —— 键照旧在）；
`--json` 给脚本与未来的 GUI 读，键与取值不本地化。"#)]
    Capture(CaptureArgs),

    /// 只读地给这台机器做一次体检：哪里会静默失效，哪里会突然全体失效。
    #[command(long_about = r#"只读地给这台机器做一次体检（`tuoen doctor`）。

**这条命令只读，而且刻意不提供任何「顺手修一下」的开关。** 它不会写 `PATH`、
不会写注册表、不会动任何工具，也不会把它报出来的东西顺手清掉
（决策 24：`PATH` 上的东西是你的）。`capture` 把状态写下来，`doctor` 判断
这份状态哪里是坏的；真正改机器的那条路是 `restore`（它默认只出计划）。

不给这类开关不是「这一版还没做」，而是这一票的判据：体检的结论要能被信，
前提是它**没有动机**把结论做得好看。（票据点名否掉的那个开关，连名字都不在
这份帮助里 —— 少一个能顺手改机器的入口，就少一处「报告是症状」与
「报告是它自己动过的痕迹」分不清的地方。）

## 每条发现五列

  id          稳定的机器可读 ID（`path.length-budget`、`tool.ghost` …）
  severity    error / warn / info —— error 是「会静默失效」或「会突然全体失效」
  message     中文一句话（**只在人类输出里**）
  evidence    具体是哪几条：PATH 条目的位置、变量名、目录
  source      这条结论从哪来（注册表 / 路径解析 / 文件系统 / 检测引擎 …）

## 退出码

**0 = 体检跑完了，哪怕发现了 error。** "发现了问题"是体检的**结论**，
不是体检的**失败** —— 报成非 0 会让每一次"看一眼这台机器怎么了"都看起来像一次
故障，于是 `|| true` 就成了习惯，而判据反而丢了。

脚本要的判据是 `--json` 里的 `counts.error`：一个不用解析人话就能读到的数字。
这也是这里**没有** `--fail-on` / `--strict` 之类开关的原因 —— 严重度本身就是数据，
要不要因此变红由消费者决定。

**1 = 跑不起来**（算不出 tuoen 自己的根位置：`%LOCALAPPDATA%` 与
`%USERPROFILE%` 都读不到）。此时用部分信封报错，并带出算出来的那两个根。

## 两个开关

  --json        机器输出：稳定、不本地化（决策 35）。`message` 不进 JSON
                （它天生是中文），机器读的是 `id` / `severity` / `source` / `confidence`。
  --no-probe    不跑「全局包前缀在哪」这类探测。默认**开**，因为它是
                `tool.global-prefix-inside-version-dir` 唯一的输入；关掉之后
                那一类结论就是「没看」，而不是「没有」。

## 一台坏机器上它先说什么

它把「`PATH` 上确认可执行」与「注册表声称已装但文件缺失」并列展示 ——
用户看一眼就会决定信不信这个工具。所以报告里每一句都带证据，
而且**没有发现时会明说「没有发现问题」，并同时印出这次看了多少**。"#)]
    Doctor(DoctorArgs),

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

    /// 看清并修改 `PATH`（`show` / `diff` 只读，`add` / `remove` / `apply` 先出计划再落盘）。
    #[command(
        subcommand,
        long_about = r#"看清并修改 `PATH`。

**不带子命令时等于 `tuoen path show`** —— 看是最安全的默认动作，而"敲了
`tuoen path` 就改了 `PATH`"是最不该发生的默认动作。

五个子命令：
  show          只读地报告现状（长度预算、重复、失效条目、遮蔽……）
  diff          只读地把本机 `PATH` 与一份目标快照**逐条**比一遍（六类 + 理由）
  add <dir>     把目录追加到**用户级** `PATH`
  remove <dir>  从**用户级** `PATH` 里删掉目录
  apply         把选中的类/条目**重建**成一份新的 `PATH` 并写回

## `diff` / `apply` 与另外三个不是一回事

`add` / `remove` 动的是**一条条目**；`diff` / `apply` 动的是**整条 `PATH`** ——
后者先把本机与目标快照逐条比出六类（`keep` / `add` / `remove` / `move` / `fix` /
`case-only`），再**重建**出一份完整的新有序列表。立场的原话是：
**"本机 `PATH` 已经是坏的，原样搬运会把旧病一起移植"**，所以这一族做的不是复制。

两条硬规矩：

* **没选中的东西连位置都不动。** 把重建实现成"按目标顺序整表替换"，`--only add`
  就会顺手重排整个 `PATH` —— 而重排就是改优先级。所以基底永远是**本机**的列表。
* **`apply` 要求你把意图说出来。** 不给 `--only` 也不给 `--pick` 就**拒绝执行**
  （`nothing-selected`，退出码 1）：默认值一旦存在，"顺手删掉几条"就会成为默认行为。

机器级的改动**算出来但不写**（逐条进 `requiresElevation`）：机器级要提权，
而 tuoen 不做"顺手提权" —— 一个弹 UAC 的 `path apply` 会让"我只是想看看 diff"
变成一次系统级改动。`--dry-run` 与真写共用同一套 `diff` + 重建 + 计划构造。

## 为什么这件事必须这么小心

`PATH` 是本项目里**唯一**一处「一次调用就能让整台机器所有命令失效」的地方：

* `setx` 在 **1024** 字符处**静默裁剪**，并把你值里所有 `%VAR%` **永久展开**成
  字面量 —— 所以 **tuoen 绝不调用 `setx`**。写回走的是一次整条值写完
  （不是逐段改：逐段改会有"`PATH` 暂时少了几个目录"的中间状态），
  然后广播 `WM_SETTINGCHANGE`。
* `cmd.exe` 在 `PATH` 超过 **8191** 字符后**完全忽略整条 `PATH`** —— 不是部分失效，
  是所有命令一起失效。这两个数字是**不同的悬崖**，混为一谈会让告警出现在错误的位置上。
  重建之后的长度是**注册表口径的下界**（不含进程注入项），所以它报 `ok` 不代表真机
  `ok`，而它报 `exceeded` 一定是真的超了 —— 那时 `apply` **拒绝写入**。

## 四件平台事实

1. 进程 `PATH` = **机器条目在前、用户条目在后**，所以用户级工具永远输掉名字冲突
   —— 这就是 shim 存在的理由（决策 10）。
2. 机器级 `PATH` 要**提权**才能写，而 tuoen **不做"顺手提权"**：`add` / `remove` /
   `apply` 只动用户级，机器级只读。
3. 进程 `PATH` 里可能有**注册表里没有的条目**（进程注入，本机实测有 PowerShell 的
   MSIX 别名）。`show` 单独把它们列出来，因为只看注册表会漏掉它们，而它们真的占长度。
4. **已经跑着的终端 / IDE 拿不到新环境**（环境块是 `CreateProcess` 时复制的）——
   写完要新开一个终端。这是平台限制，不是命令没生效。

## 只报告，不自动清理

重复条目、指向不存在目录的条目、硬编码了用户名的条目、被别的目录抢在前面的 shim
—— 这些 `show` / `diff` **全部只报告**（决策 24）。`PATH` 上的东西是用户的：
`diff` 把它们归入 `fix`，改不改由你在 `apply` 里选。拼写疑似**只报告、永不纠错**。

## `--dry-run`

`add` / `remove` / `apply` 都有 `--dry-run`，它走的是与真写**同一套**计划代码，
只是不落盘。"#
    )]
    Path(PathCommand),

    /// 起一个子 shell，把项目 pin 的版本目录**前置**进它的 `PATH`。
    #[command(
        long_about = r#"起一个子 shell，把项目 pin 的版本目录前置进它的 `PATH`。

读当前目录的 `tuoen.toml`，解析出每个工具该用哪个版本，把那些版本目录
**前置**进子 shell 的 `PATH`，然后起一个**真实的交互式 shell**。

## 它不改任何全局状态

这是本项目里最容易做错的一件事。`tuoen shell`：

* **不翻转任何 junction** —— 翻转是全局副作用：多终端场景下会让**别的**终端
  突然换版本。L0 的 `tuoen use` 是显式命令，那是另一回事。
* **不改父进程的环境** —— 环境块是 `CreateProcess` 时复制的，改不进去；
  退出这个子 shell 之后，外面的一切回到原样。
* **不动 store 里的任何东西** —— 它只读。

## 锁：不一致就停下来

`tuoen.lock` 存在且与 `tuoen.toml` 的声明不一致时，这条命令**拒绝启动**，
并告诉你跑 `tuoen lock`。理由（决策 116）：按锁执行是"你没拿到声明的那套"，
按声明执行是"锁形同虚设" —— 两条都在骗人，所以第三条：停下来。
**没有** `--ignore-lock`：先有需求再加，加容易、去难。

没有锁文件时它现场解析一次，并把结果**写成** `tuoen.lock`（进 git 的那个）。

## 遮蔽：前置了却没生效

前置之后还可能有命令**没从我们的目录里解析到**（版本目录里没有那个命令、
布局猜错了、或者 `PATH` 上另有一个更早的安装）。命中就在 **stderr** 打印明确
警告（含命令名与赢了的那条目录），**不静默继续** —— 用户以为切了版本、
其实没切，是这个功能唯一会"看起来在工作"的失效形态。

## 退出码

子 shell 的退出码**原样**透传。这一族自己的失败是 1（`missing-pin` /
`lock-mismatch` / `version-not-installed` / `shell-depth` …）。

## `--json` 必须与 `--dry-run` 一起用

不带 `--dry-run` 的 `--json` 是**用法错误**（退出码 2）：交互式子 shell 与 JSON
共用 stdout 是必然打架的，子进程会往同一个 fd 上写东西（提示符、`dir` 的输出、
PowerShell 的启动横幅）。把"计划"与"执行"分开之后，JSON 契约就永远是完整的。

## 两个开关

  --shell <cmd|powershell>  起哪一种（默认 `cmd`：它最不需要额外前提）
  --exec <COMMAND>          不进入交互式 shell，只跑这一条命令（测试与脚本用）"#
    )]
    Shell(ShellArgs),

    /// 与 `shell` 同一件事，但**只在已信任的目录里**自动应用 pin。
    #[command(long_about = r#"与 `tuoen shell` 同一件事，但多一道信任门。

`tuoen auto` 与 `tuoen shell` **共用同一个计划构造器**，差别只有那道门
（决策 121：两条路径各写一份构造逻辑必然漂移，而漂移的表现是
"`auto` 切了、`shell` 没切"）。四种情况：

  trusted        指纹与信任时一致 → 放行，与 `shell` 完全一样
  not-trusted    这个目录不在清单里 → **不启动** + stderr 提示跑 `tuoen trust`
  stale          `tuoen.toml` 改过了（指纹对不上）→ 拒绝 + 提示重新 `tuoen trust`
  missing-file   这个目录根本没有 `tuoen.toml` → 拒绝（`missing-pin`）

## 为什么默认关闭

进入陌生仓库就自动改工具链版本是**安全漏洞**：一个 clone 下来的 `tuoen.toml`
可以 pin 一个带后门的"Node 版本"。所以自动切换要用户显式信任一次，
而信任记录的是**绝对路径 + 首次信任时的指纹**（指纹是为了防"删掉目录再 clone
一个同名目录"——光记路径的话新目录会继承旧目录的信任）。

清单在 `%APPDATA%\tuoen\trust.toml`，**不在被信任的目录里**：放在里面的话，
任何 clone 下来就自带信任标记，等于没有机制。

## 它也不注入任何 shell profile

"进目录就自动切"需要改用户配置（PowerShell profile / `cd` 钩子），那是独立议题 ——
改用户配置要有自己的 plan/diff/apply 与警告。L1 的自动切换是用户**显式**敲
`tuoen auto` 触发的。

参数与 `tuoen shell` 完全一致（含 `--json` 必须配 `--dry-run`）。"#)]
    Auto(AutoArgs),

    /// 管理信任清单：哪些目录被允许自动应用 pin。
    #[command(long_about = r#"管理信任清单：哪些目录被允许自动应用 pin。

```text
tuoen trust                  信任当前目录（算 `tuoen.toml` 的指纹、写清单）
tuoen trust --list           列出每一条 + 它**现在**还有不有效
tuoen trust --revoke <PATH>  摘掉一条
```

## 清单在哪

`%APPDATA%\tuoen\trust.toml` —— **用户级中央清单**，记绝对路径 + 首次信任时的指纹。

* 信任标记**绝不放在被信任的目录内**：那样任何 clone 下来就自带信任标记，
  等于没有机制。
* 指纹是因为**路径可能被替换**（删掉目录再 clone 一个同名目录）：
  光记路径的话，新 clone 来的那个目录会继承旧目录的信任。

写盘是**临时文件 + rename** 的原子替换：这份文件被两个终端同时改的代价是
整份信任清单消失（或半截 TOML）。

## `--list` 会重算每一条的状态

`trusted` / `stale` / `missing-file` 是**现场重算**的，不是写盘时记下来的 ——
一个把过期条目显示成"已信任"的清单是在说谎，而它恰好是用户唯一的查看入口。

## `--revoke` 摘不到时会说 `absent`

不是假装成功，也不是失败：撤销的目标状态（"这个目录不在清单里"）已经成立，
但"我摘的那条到底在不在"正是用户要问的，所以它有一个自己的取值。
摘不到时**不写盘** —— 一次无谓的原子替换会让这份文件多一次没有意义的变更。

## `%APPDATA%` 读不到就失败

错误码 `unwired-root`。宁可失败，也不要往一个相对路径里写信任清单：
相对路径的含义随当前目录变，而信任清单必须**不随 cwd 变**。"#)]
    Trust(TrustArgs),

    /// 把 `tuoen.toml` 的声明解析成一份可提交的 `tuoen.lock`。
    #[command(
        long_about = r#"把 `tuoen.toml` 的声明解析成一份可提交的 `tuoen.lock`。

`tuoen.toml` 是**声明**（"我要 Node 24"），`tuoen.lock` 是**解析结果**
（"这台机器上 24 指的是 24.19.0，它在哪、从哪来、哈希是多少"）。两者都进 git。

锁的全部价值在"不一致"那一侧：改了 `tuoen.toml` 却忘了重新 lock 时，
`tuoen shell` / `tuoen auto` 会**当场拒绝启动**并让你跑这一条命令 ——
而不是让某个人在别的机器上悄悄拿到另一套工具链。

## 锁里没有时间戳

解析两次**逐字节相同**。时间戳会让每次 `tuoen lock` 都产生一行 diff，
而那一行不携带任何信息 —— 一个每天都要在 code review 里被忽略的 diff，
等于训练人忽略这份文件。

## `--dry-run`

走**同一套**解析与组装代码，只是不落盘。`--json` 里的 `written` 会如实说
`false` —— 那不是"写失败了"，是"这一次的任务里没有写"。

`--json` 在这里**不需要**配 `--dry-run`：这条命令不启动任何子进程，
stdout 上没有第二个写者。"#
    )]
    Lock(LockArgs),

    /// 照着另一台机器的 `tuoen.d/` 快照还原本机（默认只出计划）。
    #[command(long_about = r#"照着另一台机器的 `tuoen.d/` 快照还原本机。

**不带开关 = 只出计划，一个字节都不写。** `restore` 是本项目里后果最重的命令：
它会装工具、写用户级环境变量、重建用户级 `PATH`，而它要动的恰恰是别的进程
此刻正在读的那份环境。所以「动手」必须是明说的：

  tuoen restore                    只出计划（默认）
  tuoen restore --dry-run          同上，只是把「我只要计划」说出来
  tuoen restore --apply            真的做

`--apply` 与 `--dry-run` 同时给是矛盾，会在解析期被拒（退出码 2）。

## 四节，各自独立

  tools   缺失且**可复现**的工具走安装流（需要网络）
  path    复用 `tuoen path diff` 的判据重建用户级 `PATH`
  env     用户级环境变量的**整值写**（一次广播）
  wsl     只报告（装发行版会改 Windows 功能并要重启，那不是顺手该做的事）

`--only <section>` 可以重复。指向快照里没有的节、或被排除的节，都会在计划里
记成 `skipped`，但**说明不同**（"快照里没有它" vs "你没选它"）。

## 机器级与凭据：算出来，但不做

机器级的改动（`HKLM` 下的环境变量、机器级 `PATH`）只算不写，逐条进
`manualActions` —— **tuoen 不自动提权**（决策 136）。凭据同理：快照里记的是
"需要哪个凭据"，材料从来不进快照（DPAPI 是用户+机器绑定的，搬过去会静默失败）。

所以「无变更」**不等于**「你什么都不用做」：`manualActions` 与 `has_changes()`
无关，永远照打。

## 退出码

  0  计划出来了 / `--apply` 全部做成（**包括"无变更"**）
  1  跑不起来（快照读不了、空快照），或 `--apply` 里有节没做成
  2  用法错误（`--apply` 与 `--dry-run` 同时给、`--only` 取值不认识）

`--json` 的 `data` 就是计划那四个键（`snapshot` / `sections` / `manualActions` /
`summary`），`--apply` 时多一个 `apply` 对象。`snapshot` 是**用户敲的那个路径原样**。"#)]
    Restore(RestoreArgs),
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
