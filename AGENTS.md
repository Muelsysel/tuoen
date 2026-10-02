# tuoen · 拓境 — Agent 工作约定

本文件是本仓库的 agent 约定入口。**读它，然后读它指向的文件。**

## 项目一句话

`tuoen`（拓境）是一个面向 Windows 的开源开发环境管理器，招牌能力是**整机开发状态（dev-state）的捕获与还原**，而非又一个包安装器。Rust 实现，MIT OR Apache-2.0。

## 先读这个，再动任何东西

**`docs/DESIGN.md` 是必读，不是参考。** 它是七轮设计拷问的决策日志（最初 35 条，实现期继续追加：L0 收尾时 83 条，L1 的 #12 之后是 94 条，#13 之后是 112 条，#14 之后是 123 条，#15 之后是 149 条，#16 之后是 164 条）。它是本仓库里若干"本来很合理"的设计被**禁止**的原因——例如：

- **绝不用 `setx` 写 `PATH`**（1024 字符裁剪 + 永久展开 `%VAR%`）
- **绝不断言 symlink 可创建**（未提权且未开 Developer Mode 时文件与目录都失败）
- **绝不发 `.cmd` / `.ps1` shim**（Node ≥18.20.2 后无法被 spawn）
- **绝不改写 winget / Scoop 的 `PATH` 与状态**
- **绝不捕获密钥材料**（只捕获意图）
- **绝不自动清理幽灵条目**（只报告）

违反其中任何一条之前，先读它对应的决策与理由。

## Agent skills

### Issue tracker

Issues, specs and tickets live as GitHub issues in `Muelsysel/tuoen`, operated through the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles keep their default label strings (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`), plus `spec` and `wayfinder:*`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and one `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## 开发流程

本仓库遵循 mattpocock/skills 的主流程。完整路由见 `/ask-matt`。核心链条：

```
/setup-matt-pocock-skills  →  /grill-with-docs  →  /to-spec  →  /to-tickets  →  /implement-spec
```

**上下文卫生**：`/grill-with-docs` → `/to-spec` → `/to-tickets` 必须在**同一个不中断的上下文窗口**里完成（不要在 `/to-tickets` 之前 compact 或 clear），这样拷问、spec 和票据建立在同一套思考上。之后每个 `/implement` 才开新窗口。

## 领域语言

术语以 `GLOSSARY.md` 为准。**不要漂移到它明确避免的同义词**——例如"票据"（ticket，来自 `/to-tickets` 的垂直切片）与"issue"（tracker 上的条目）在本仓库是不同概念；"junction" 与 "symlink" 是**不可互换**的两种 reparse point。

## 中文优先

- 默认界面中文，中文文档优先，英文 README 并行
- **但 `--json` 输出必须稳定、不本地化**——脚本化与 GUI 都依赖它
- **永远不要匹配错误文本**（本机 zh-CN，PowerShell 的符号链接报错是本地化的：`此操作需要管理员权限。`），要匹配 HResult / Win32 码

## 平台现实

代码运行在 Windows 上，且**默认非提权**。动手前必须知道的地面事实（详见 `docs/DESIGN.md` §2）：

- 进程 `PATH` = 机器条目在前、用户条目在后 → 用户级工具**永远输掉名字冲突**，只能靠 shim 抢名字
- `cmd.exe` 在 `PATH` 超过 **8191 字符**后**完全忽略**它（整条 PATH 一次性全部失效）；`setx` 在 **1024** 处裁剪
- 相对路径**永远**受 `MAX_PATH`(260) 限制——`\\?\` 无法加前缀
- `PATH` 上 29% 的条目含空格，最糟的同时含空格和版本号
- 归档：系统自带 `tar.exe` 是 bsdtar 3.8.8（zip/tar.gz/tar.xz/tar.zst/tar.bz2 **以及 7z** 都能走它——7z 那条是**实测**的，见 `research/BSDTAR_SAFETY_MEASURED.md`）
- **bsdtar 对路径逃逸可靠（`..`/symlink/硬链接/设备全拒，且退出码变 1），但对名字的危险形态不可靠**：`CON` 真的会被创建且普通路径看不见、`trailing.` 删不掉、`:` 被静默改名、大小写碰撞静默覆盖。所以解压必须自己校验条目名

## 构建这台机器（三个实测发现，别重新踩）

**本机没有 MSVC Build Tools —— `link.exe` 不存在。** 所以只能走 `x86_64-pc-windows-gnu`。以下三条都是实测出来的，不是推断：

1. **`rust-toolchain.toml` 必须写完整 host 三元组**（`channel = "stable-x86_64-pc-windows-gnu"`）。只写 `"stable"` 时 rustup 按 `Default host`（= msvc）解析，host 侧的构建脚本与 proc-macro 会失败：`error: linker link.exe not found`。
2. **rustup 的 `rust-mingw` 组件不提供可用的 `dlltool`。** `windows-sys` 在 GNU target 上需要它生成 import library，而 rustup 自带的是垫片（旁边的 `GCC-WARNING.txt` 说那个 gcc 只能当链接器用），会报 `dlltool could not create import library …: CreateProcess`。已装一份用户级 MinGW-w64 在 `C:\Users\Muelsyse\.local\toolchains\mingw64`（WinLibs，不需要管理员）。
3. **rustc 在 GNU target 下不认 `DLLTOOL_<target>` 变量，它就在 `PATH` 上找 `dlltool.exe`**；而 cargo 的 `[env]` 对已存在的变量默认不生效（`force = true` 时也不是前置而是覆盖）——两种写法都实测失败。所以 MinGW 的 `bin` 在 `HKCU\Environment\Path` 里（只读、非提权、`REG_SZ` 保持不变），linker 路径留在 `.cargo/config.toml` 的 `[target.x86_64-pc-windows-gnu]`。

`cargo` 与 `rustc` **不在**默认 `PATH` 上，用 `$env:USERPROFILE\.cargo\bin`。

## 会启动进程的代码：三条铁律（两次事故换来的）

本仓库已经有**两次**因为探针/测试启动进程而把整机拖垮的记录。两次都不是"逻辑写错"，
而是**失效模式的代价不对称**：一个名字、一个字节的差错，换来整机失去响应。

1. **绝不允许任何程序启动它自己。** 第一次：一个测试让 shim 的**落盘路径与目标路径是
   同一个文件**，生成出的转发器"目标是它自己"，运行一次就无限自我启动，堆到 **20580 个
   进程**。第二次：Ctrl-C 探针的宿主"fork 自己"进入子模式，而 `wide()` 已经带了 NUL
   结尾、参数被拼在了那个 NUL **后面**，于是子进程收到零个参数、回到宿主模式又 fork
   一次 —— **409 个进程**，每个还带着一个不可见控制台和继承来的 stdout 管道。
   现在的做法是：宿主与执行者是**两个不同的二进制**，宿主在启动前断言"执行者不是我"。
   需要两个身份时，写两个二进制，不要重跑自己。

2. **构造命令行只留一个函数，参数必须拼在结尾的 NUL 之前。** 第二次事故的根因就是
   "半成品缓冲 + 自己补 NUL"。让调用方拿不到半成品，这个 bug 就没有藏身处。
   同类危险还有：前缀参数里混进 U+0000 会把命令行在那里截断，目标带着比预期少的参数
   启动且不报错 —— `ShimSpec::validate` 现在直接拒掉它。
   **唯一允许"原样塞进去"的地方是 `cmd.exe /C` 的载荷**（`CommandExt::raw_arg`）：它的
   解析规则**不是** MSVCRT 的规则，`Command::arg` 的 `\"` 转义它不认 —— 真机实测
   `tuoen shell --exec 'node -e "console.log(1)"'` 在 `Command::arg` 下**输出为空、退出码 0**
   （一句看起来完全合理的成功），而 `--exec "node -v"`（不含引号）照常工作，所以只看
   "能不能跑"是发现不了的。代价是**"载荷里可以有任何字符"这件事由调用方负责**，而调用方
   只有 `launch_command()` 一个（`crates/cli/src/shell_cmd.rs`）；PowerShell 那条必须继续
   走普通 `arg`（它是正常程序，`raw_arg` 反而会拆坏）。判据是 `echo "hi"` 必须印出 `"hi"`
   且不许出现 `\"`。

3. **跑之前先算最坏情况下的进程数，给它上限，跑完清场核对。** 探针要能在"转发器死了而
   子进程还活着"这种局面下自己收场（`WaitForSingleObject` 带超时，不用 `INFINITE`）。
   **一条不能失败的测量不是测量。**

## 会改用户系统状态的代码：七条规矩（前五条来自票据 #8，第六条来自 #9，第七条来自 #15）

`PATH` 是本项目里**唯一**一处"一次调用就能让整台机器所有命令失效"的地方，所以动它的代码
比别处多守七条。它们的共同点：**不是"写得更小心"，而是把危险操作围在证据里**。

1. **绝不调用 `setx`。** 它在 **1024** 字符处静默裁剪，并把值里所有 `%VAR%` **永久展开**
   成字面量。写 `PATH` 只有一条路：`RegSetValueExW` 整条写回 + 广播
   `WM_SETTINGCHANGE`，且**值含 `%` 才用 `REG_EXPAND_SZ`**（否则保留原类型，再否则 `REG_SZ`）。
   验收脚本用 grep 钉住这件事（判据是**带引号**的 `setx` 字面量 —— 源码里到处都用反引号
   写 `` `setx` `` 来警告不许调用它）。

2. **读用户的环境变量不许展开。** 低层走 `RegEnumValueW`（它**从不**展开 `REG_EXPAND_SZ`）。
   展开是**不可逆的信息损失**：只有不展开读，才区分得出"值本来就是这样"与"被 `setx`
   展开过"。推论：含 `%` 的条目**不许**判成"失效"—— 我们没展开它，答不了的题不许猜。

3. **先出计划，再落盘；`--dry-run` 走同一套计划代码。** 另写一条只读分支的话，
   dry-run 通过只能证明那条分支对。验收里钉这件事的办法是断言
   `run(dry_run=true)` 与 `run(dry_run=false)` 产出**同一个计划对象**。
   计划与现状一致时**不写、也不广播** —— 广播是全局副作用（打到每一个顶层窗口）。

4. **危险的探针必须自证它没碰不该碰的东西。** 真机探针一边写 `TUOEN_PATH_PROBE`
   做类型往返，一边在开头与结尾各快照一次两个作用域的 `Path`（类型 + 原始字节），
   `before == after` 必须为真并写进机器可读的 `SUMMARY` 契约行。**声称没碰 ≠ 证明没碰**，
   而且这份证据要能被机器检查。测试同理：`cargo test` 永远不许碰真实
   `HKCU\Environment` / `HKLM` —— 需要假注册表就给假注册表，**不给出厂二进制留后门开关**。

5. **测试断言不许依赖机器状态，只能断言"跑完前后没变"。** "真实的 `%LOCALAPPDATA%\tuoen\shims`
   不存在"这种断言是**机器状态**，不是被测代码的性质：任何人真的用过 `tuoen shim add`
   都有这个目录，于是它会在最不该红的时候红（票据 #8 的真机验收就把它踩红了一次）。
   正确形状是"跑之前列一遍、跑之后再列一遍、逐项相同"——而且**要往被测代码会写的那个目录里
   看一层**：只列顶层的话，"在真实 shim 目录里写了一个 shim"这种最严重的事故看不见，
   因为那个目录名本来就在那儿。这是决策 50 的第二次教训：第一次是"断言不会失败"
   （假的通过），这一次是"断言会被误报"（假的红）。

6. **"新终端里生效了"只能从注册表重建一份环境来验，绝不在当前这个 shell 里再跑一次。**
   环境块是在 `CreateProcess` 时从父进程复制的，注册表只在 shell 建立自己的环境时被读一次
   —— 所以**在当前 PowerShell 里再敲一次 `node -v`，能证明的东西恰好是零**，它拿到的还是
   启动时那份旧环境。做法是 `scripts/fresh-terminal.ps1`：不展开读两个作用域的 `Path`，
   拼成"机器级 + 用户级"，只塞进子进程的环境块（这正是登录时 explorer.exe 做的事）。
   另外两条同源的经验：**判定"谁赢了名字冲突"只能看 `where` 的第一个命中，版本号不是证据**
   （本机两个 Node 版本号恰好相同，"对上了"是假象）；**安装耗时必须分开报"缓存命中"与
   "冷下载"**（只报快的那一次是误导）。

7. **改状态的脚本必须"先自证还原能用，再允许写入跑"。** 把当前值**原样写回一次**
   （同字节、同类型、走与还原**完全相同**的代码路径），断言哈希与类型逐字未变；还原之后再
   **当场**复核一次哈希，不一致就红字喊出来并指出备份在哪。**这一条是 #15 真机验收里最严重的
   一次事故换来的**：验收脚本的 `Set-PathValue` 用的是 `(Get-Item 'HKCU:\Environment').SetValue(...)`，
   而 PowerShell 的注册表 provider 返回的是**只读**句柄 → `Cannot write to the registry key.`，
   于是"`--only add` 真的写成功（808 字符、新终端里 `mvn` 找得到）**而还原抛异常**"这个最坏的
   组合真的发生过一次，`HKCU\Environment\Path` 被留在改动值上，直到按跑前手工备份用
   **可写子键句柄**（`[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)`）
   写回并逐字核对哈希。**"我们打算还原"和"还原真的能写"是两件事** —— 前者写在代码里，
   后者必须被证明；**一条不能失败的还原不是还原**（与铁律 3 同源）。

## 会写报告与快照的代码：四条规矩（来自票据 #12 的真机验收）

`capture` 这类只读命令看起来没有风险 —— 它的风险全在**说出去的话是不是真的**。
四条规矩的共同点：**一句看起来完全合理的错话，比一次崩溃更难被发现**。

1. **"不是一条路径"的判据要覆盖列表。** `;` 分隔的值（`Path` / `PSModulePath`）不是
   **一条**路径，这条判据必须排在"像不像绝对路径"**前面**：只判前缀会把整条 `;` 串拿去
   问磁盘，答案恒为"不存在"——真机上 4 条"目标不存在"里有 3 条是这么来的，而它会直接
   误导 `doctor`（剩下那 1 条才是真的：变量指向已经卸载的软件）。
2. **空段落不是一个缺失的目录。** `;;` 那种空条目"在不在"这个问题本身不成立：报
   `unknown` 并带上 `empty = true`，判"失效条目"的判据是 `!empty && exists == "no"`。
   一个**永远修不好**的发现会训练用户忽略所有发现。
3. **字段的语义以票据为准，不以"代码与自己的文档自洽"为准。** `dup_index` 曾被实现成
   "不同的值依次编号"、注释也照写了一遍，两处一致、都好看，而真机会印 32 而不是 18。
   任何"我按更好理解的方式实现、并把这个理解写进文档"的时刻都属于这一类。
4. **验收脚本自己写的每一句话也要能被独立复核。** 核对 `reparse` 分布时用错了枚举拼法
   （`symlink` vs 真机上的 `symlink-dir`），差一点把"采集器没认出符号链接"报出去 ——
   **假的差异与"代码错了"长得一模一样**。所以脚本里每个数字都要与**它自己从注册表/磁盘
   数出来的那一份**对照，而不是与产品自己的输出对照；`-SelfTest` 必须能让它 exit 1。

## 会下结论的代码：五条规矩（来自票据 #13 的真机验收）

`doctor` 的风险不在"算错"，而在**说错**：把正常的东西报成坏的（用户立刻不再信任这个工具），
或者把"不知道"写成"没有"（用户据此做了一个错决定）。五条规矩的共同点与上一条一样：
**一句看起来完全合理的错话，比一次崩溃更难被发现**。

1. **事实与判断分开。** `collect_facts(ctx, opts) -> MachineFacts` 是**唯一**碰机器的地方（只读），
   `diagnose(&MachineFacts) -> Vec<Finding>` 是**纯函数**。检查项拿不到机器 —— 于是"测试必须用
   固定装置、不许用本机实时状态"变成**架构**而不是纪律（票据把它列为硬性约束）。
   检查项里出现一次 `ctx.fs` 就说明这条分界破了。

2. **`evidence` 是数据，`message` 是散文。** evidence 里只许有"位置/名字 + 值"
   （`machine#21`、`tool=java command=javac dir=…`），**且必须是纯 ASCII**（验收脚本逐行断言）；
   中文只进 `message`，而 `message` 不进 `--json`。**检测引擎给「人」看的占位符不许原样抄进
   evidence** —— `path=<无 InstallLocation，卸载键 {GUID}>` 曾经真的漏进了成功载荷，是被
   "成功载荷无 CJK"那条契约用例当场抓住的。翻译成 `path=<placeholder>` + 独立的
   `uninstall-key={GUID}` 才进证据。

3. **"不知道"是三态里的第三态。** `Some(true)` / `Some(false)` / `None` **绝不许合并**：
   注册表键不存在 ≠ 开关关着（本机 `AllowDevelopmentWithoutDevLicense` 就是"不存在"）。
   同一件事在本仓库已经有三次教训：`exists` 三态、`target_exists` 四态、这里的 `system.*`。

4. **判据太宽 = 噪声源，而每次收窄都要补一个反例用例。** `tool.multi-manager` 第一版按
   "来源种类 ≥ 2"判，于是 git/dotnet/wsl 全被报成 error —— 而"一个工具既在 `PATH` 上又在
   卸载键里"完全正常；改成只数**真正的管理器**（`manager` / `tuoen`），6 条降到 1 条。
   `env.duplicated-scope` 同理：`Path`/`TEMP`/`PATHEXT` 在两个作用域都有是**平台设计**，
   报它就是把正常说成腐坏（5 条降到 2 条）。**"提到"（`path-resolution`/`registry-arp`/
   `filesystem-scan`）与"管"是两个量**，混在一起会印出自相矛盾的证据。

5. **"没变化"的断言最容易变成"什么都没比"。** `strip_timestamp` 按**行**删时间戳，而
   `--json` 的成功载荷是**一整行** —— 删行等于删掉整份载荷，于是"两次逐字节相同"退化成
   "两份**空**串相同"（一个把两边都删光的断言永远通过），它平时能过只是因为两次调用恰好
   落在同一秒。断言幂等/没变化时，必须问一句：**这条断言失败过吗？** 修法：只替换值、
   保留键与引号（形状也是契约），并补一条**在旧实现下必须红**的用例。

顺带两条写验收脚本的坑（都在 #13 里踩过）：`(Findings-Of 'x').Count` 在**空数组**上会报
"找不到属性 Count"（PowerShell 把空数组枚举成"什么都没有"，调用方拿到 `$null`）——写
`@(…).Count`；`$array -like 'p*'` 是**逐元素**匹配，拿它当过滤器会把整组都算成命中。
比较两个集合时两边都要**先排序**（决策 94 的第三次）。

**#15 又添了四条，全是"脚本自己造出来的假结论"**（详见
`docs/acceptance/L1-15-path-rebuild.md` 的 §4；本票的设计决策是 `docs/DESIGN.md` §1.17–§1.18 的 126–149）：

1. **`@($null).Count` 是 1，不是 0。** 取一个可能不存在的键（`Prop`）或一个可能是空数组的键，
   用 `@(...)` 包起来就"数出 1 条"——于是"产品只输出了一行"这种假差异会同时出现在五条断言上。
   要数条数一律走 `Arr`（先滤 `$null` 再 `@()`）；**函数返回空数组时调用方拿到的是 `$null`**，
   所以 `$x = Arr …` 之后还要 `@(Arr …)` 才安全。
2. **成功载荷有一层 `data` 信封。** `--json` 的形状是 `{schemaVersion, command, ok, data:{…}}`，
   而失败载荷的 `error` 在**信封上**。断言一律从 `data` 里取；第一版每条成功断言都读信封，
   `Prop $j 'rows'` 返回 `$null`，再叠加上第 1 条，就成了"产品 1 / 脚本 50"。
3. **"位置不变"是子序列，不是同下标。** `--only fix` 会删掉重复与空条目，后面的条目自然往前挪；
   按"同下标逐字相同"比，会在**做对了**的时候报红。同理，拿"after 的条数"去比
   "本机条数 − remove 类条数"是把"该删的"和"不该删的"混成了一个数。
4. **机械替换会改写它自己要引入的那个函数。** 用正则把 `@(Prop X 'y')` 换成 `Arr X 'y'` 时，
   新写的 `Arr` 定义体里那一行 `@(Prop $Obj $Name | …)` 也被同一条正则命中，
   于是 `Arr` 变成"调用自己" —— 报错是"由于调用深度溢出，脚本失败"。批量替换后必须**回头读一遍
   被替换区域**，而不是只看替换条数。

还有一条**关于数字的**：`docs/acceptance/L1-15-path-rebuild.md` 里同一个量出现两次时，
**两个分母都要写出来**（预算余量 6376 是"加了 maven 那条之后"的计划、6411 是"原始快照"）。
我一度把 6376 当成过期数字改成 6411 —— 错的是改动，不是原数字。

**#16 又添了三条**（详见 `docs/acceptance/L1-16-restore.md` 的 §4）：

1. **空对象上的成员枚举会炸，而"空"往往是最正常的形态。** `$obj.PSObject.Properties.Name`
   这个惯用法在 `effective = {}`（"没有任何要写的条目"）上报
   `在此对象上找不到属性"Name"`，整条验收脚本当场中止。写 `Prop` / `PropNames` 这类助手，
   对每个 `PSPropertyInfo` 逐个取 `Name`。与 #15 的 `@($null).Count == 1` 同源：
   **"空"既不是"没有这个键"，也不是"有 1 个"。**
2. **`[0]` 落在可能为空的数组上，会让脚本"消失"而不是报 FAIL。**
   `@(… | Where-Object { … })[0]` 在数组为空时抛 `Index was outside the bounds of the array`
   —— 于是"产品少报了一节"这种最该被看见的回归，表现为脚本中止、连一条 FAIL 都打不出来。
   取"某 id 的那一节"一律走一个找不到就返回 `$null` 的助手（`Sec`），让断言去报 FAIL。
3. **验收脚本的期望值必须从决策表推出来，不能凭直觉。** #16 里我三条期望全错、产品全对
   （`wsl` 带 `missing-distro` 仍是 `no-change` + `report-only`；`summary.wouldChange` 不含它；
   `--with-fix` 时机器级 fix 让 path 变成 `requires-elevation`）。**期望值是关于设计的断言**，
   写它之前回去读决策那一行；三条错的全被我补进了决策表（161/164），而不是"改代码去迎合脚本"。

顺带两条 PowerShell 的小坑（同一个脚本里踩的）：`DirectoryInfo` **没有** `Length` 属性
（`Get-ChildItem` 出来的目录，`$_.Length` 在 StrictMode 下是终止错误）；**`-join` 的优先级高于
`-eq`**，`(a,b,c) -join ',' -eq '0,0,0'` 先拼串再比较，会把**正确**的形态报成 FAIL。

## 遇到实现问题的第一参考对象

**`mise`**（Rust、MIT、34.5k★、周更）——尤其是它的 Windows shim 处理、PATH 管理、`bootstrap` 资源抽象，以及 issue 区对已知 Windows 坑的记录。这是项目所有者明确指定的。
