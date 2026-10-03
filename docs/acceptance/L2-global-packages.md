# 票据 #27 —— L2 真机验收（全局包的根、两个来源、重定向、真装、幂等、shim）

> 这一票写的是**证据**，不是功能：`crates/**` 一个字节都没动。改的只有
> `scripts/acceptance-L2-27.ps1` 与 `docs/acceptance/L2-*`。

**结论一句话**：L2 的四个面（只读视图 / `shell` 重定向 / 真装与幂等 / shim）在作者本机上
**全部可用**，验收 PASS；仍有三处**诚实未覆盖**（见 §7），最要紧的一处是 §7b 只在
`-WithNetwork` 下跑。

## 0. 怎么跑、跑出了什么

```
pwsh -NoProfile -File scripts/acceptance-L2-27.ps1                 # 默认，离线；工件跑完即删
pwsh -NoProfile -File scripts/acceptance-L2-27.ps1 -WithNetwork    # 多跑 §7b（pip 的 shim）
pwsh -NoProfile -File scripts/acceptance-L2-27.ps1 -SelfTest       # 必须 exit 1
```

| 跑法 | 逐字输出 | SUMMARY | 退出码 |
| --- | --- | --- | --- |
| 默认（离线） | [`L2-run.txt`](L2-run.txt) | `checks_passed=104 checks_failed=0 checks_skipped=1 verdict=PASS` | 0 |
| `-WithNetwork` | [`L2-run-network.txt`](L2-run-network.txt) | `checks_passed=114 checks_failed=0 checks_skipped=0 verdict=PASS` | 0 |
| `-SelfTest` | [`L2-selftest.txt`](L2-selftest.txt) | `checks_passed=104 checks_failed=1 checks_skipped=1 verdict=FAIL` | 1 |

其它落盘的逐字输出：[`L2-globals-list-human.txt`](L2-globals-list-human.txt)（人类面）、
[`L2-shell-outputs.txt`](L2-shell-outputs.txt)（`tuoen shell --exec` 的四组 stdout/stderr）、
[`L2-timings.txt`](L2-timings.txt)（12 个命令的真实耗时）。

## 1. 票据"必须做到的"逐条对照

| 票据要求 | 落在哪 | 结果 |
| --- | --- | --- |
| §0 构建 | §0 | `-SkipBuild` 时会**断言** `target\release\tuoen.exe` 不比最新源码旧（见 §4 第 1 条） |
| §1 tuoen 根 / 机器自己的 prefix / `where python` 第一条 / 两个作用域的 `Path` 与哈希 | §1 | 全绿；`where python` 那一条见 §2 |
| §2 `globals list`（两个来源 + 只读）| §2 | 全绿；12 行 = 机器 10 + tuoen 2 |
| §2 `capture --only globals`（只出 `source = "machine"`，npm 7 / pip 3）| §3 | 全绿；**脚本自己数出来**的 7/3 与产品逐字相同 |
| §3 `%TEMP%` 里造小快照 → 计划 → `--apply` → 断言逐包 `result`/目标路径/`globals-prefix-moved` | §5 | 全绿（计划默认就是 dry-run，决策 150；脚本先跑计划再跑 `--apply`）|
| §4 `globals list` 能看到 `source = "tuoen"`；第二次 `restore` 是 `no-change` | §6 | 全绿 |
| §5 `<shims>\<命令>.exe --version` 与真身逐字相同；`.cmd`/`.ps1` 一个都没生成 | §7 + §7b | 全绿（npm 四条 + pip 一条）|
| §6 `shell --exec "npm config get prefix"` → tuoen 的根；`--exec "pip list --user --format=json"` → 与报告一致 | §4 | 全绿（后者新增，见 §5）|
| §7 安全红线 | §8 | 12/12 全绿（见 §8）|
| §8 `setx` 两段判据 + 反向验证 | §8 | 全绿（产品源码 0 处、本脚本 0 处会执行、判据反向验证会红）|
| §9 SUMMARY + `-SelfTest`（必须 exit 1）| §10 | 见 §0 的表 |
| 结论要诚实 | 本文件 §7 | —— |
| 发现的问题各自开新 issue | 本文件 §6 | 开了一条（`already-present` 不补 shim 的缺口）|
| 写入范围只有 `docs/` + `scripts/` + 评论 | `git status` | 是 |

## 2. 真机事实（跑之前那份真相，全部由脚本自己量）

```
[ok]   机器级 PATH 可读  chars=1007 kind=String
[ok]   用户级 PATH 可读  chars=773 kind=String
[note] 跑前 HKCU\Environment 共 14 个值；用户级 Path 773 字符 sha16 a12fc582e90513a3
[ok]   跑前注册表里没有 NPM_CONFIG_PREFIX  user=0 machine=0
[ok]   跑前注册表里没有 PYTHONUSERBASE  user=0 machine=0
[ok]   跑前注册表里没有 PIP_USER  user=0 machine=0
[ok]   跑之前当前进程环境里没有 NPM_CONFIG_PREFIX  value=
[ok]   跑之前当前进程环境里没有 PYTHONUSERBASE  value=
[ok]   跑之前当前进程环境里没有 PIP_USER  value=
[ok]   机器侧 npm prefix 可读  prefix=C:\nvm4w\nodejs
[ok]   node 版本可读  node=v24.19.0
[note] tuoen 的 npm 根预期在 C:\Users\Muelsyse\AppData\Local\tuoen\globals\npm\v24.19.0（从 node -v 自己算的，不是抄产品的）
```

**三个重定向变量必须**：既不在注册表里、也不在**当前进程环境**里 —— 后者是这次补上的：
只查注册表会漏掉"某个上层 shell 已经把它们设进了进程环境"这种局面，而那种局面下
"`tuoen shell` 设了它"与"它继承来的"看起来一模一样。

### 2.1 `where python` 的第一条**不是** Python（票据 §1 点名要的）

```
[note] where python 的第一条：C:\Users\Muelsyse\AppData\Local\Microsoft\WindowsApps\python.exe（0 字节，App Execution Alias）
[ok]   where python 的第一条是 WindowsApps 里的 0 字节别名占位（真 Python 在第二条）  path=…\WindowsApps\python.exe size=0
[ok]   where python 至少还有第二条（真的那份 Python）  count=2
```

它是 **0 字节 + ReparsePoint**、跑起来 **exit 9009** 的 App Execution Alias；真 Python 在
`…\Programs\Python\Python312\python.exe`（第二条）。这条正是"绝不执行别名"与
"`where` 的**第一条**才是谁赢了名字冲突"两条规矩的真机现场 —— 所以脚本里那条检查
连"它是什么"一起记，而不只是记路径。

### 2.2 机器侧包：脚本自己数出来的两份

```
[ok]   我自己数出机器侧 npm 包  count=7
[ok]   我自己数出机器侧 pip 包  count=3
[ok]   机器侧 npm 行数 = 我自己数的 7  product=7 mine=7
[ok]   机器侧 pip 行数 = 我自己数的 3  product=3 mine=3
[ok]   机器侧 pnpm 的 binNames 是四个命令  bins=pn,pnpm,pnpx,pnx
```

`npm ls -g` 与 `pip list` 是脚本独立跑的（不是抄产品的输出），再与产品的行数对照。

## 3. 逐字证据（按节）

### 3.1 §2 `globals list`：`roots` 与 12 行

```
[ok]   信封的 schemaVersion = 2（决策 189 的删除 + 决策 208 的 ok 语义）  schemaVersion=2
[ok]   信封里没有 error 键（成功载荷不出它）  keys=schemaVersion command ok data
[ok]   roots 里两个工具都在（空的根也要出）  count=2
[ok]   npm 的根 = %LOCALAPPDATA%\tuoen\globals\npm\<node -v>（我自己算的）  product=…\v24.19.0 mine=…\v24.19.0
[ok]   每一行都有 tool/name/version/source 四个键  rows=12
```

人类面（[`L2-globals-list-human.txt`](L2-globals-list-human.txt) 全文）：

```
tuoen 管着的根（`%LOCALAPPDATA%\tuoen\globals`）：
  · npm C:\Users\Muelsyse\AppData\Local\tuoen\globals\npm\v24.19.0 —— 在，1 个包
  · pip C:\Users\Muelsyse\AppData\Local\tuoen\globals\pip —— 在，1 个包
  ...
  共 12 个包：机器自己的 10 个 / tuoen 管的 2 个。
  （`?` = 拿不到命令名，不是「它没有命令」；那种包在 `--json` 里 `binNames` 键整个不出现。）
```

### 3.2 §3 `capture --only globals`：`source` 键与四种 slug

```
[ok]   globals.toml 的行里有 source 键（加性、永远出键）  keys=packages,prefix,prefix_inside_version_dir,source,tool,tool_version
[ok]   globals.toml 里 npm 那行 prefix_inside_version_dir = true  rows=1 inside=True
[ok]   globals.toml 里 pip 那行 prefix_inside_version_dir = false  rows=1 inside=False
[ok]   从 pip --version 能独立反推出 pip 的 prefix  line=pip 25.0.1 from C:\Users\Muelsyse\AppData\Local\Programs\Python\Python312\Lib\site-packages\pip (python 3.12)
[ok]   pip 那行的 prefix = 反推出来的那个目录
```

### 3.3 §4 `tuoen shell`：重定向真的到了子进程

```
[ok]   子进程里 npm 的 prefix 指向 tuoen 的根  child=…\tuoen\globals\npm\v24.19.0 expect=…\v24.19.0
[ok]   子进程里 NPM_CONFIG_PREFIX 真的设上了  out=NPM_CONFIG_PREFIX=C:\Users\Muelsyse\AppData\Local\tuoen\globals\npm\v24.19.0
[ok]   子进程里 PYTHONUSERBASE 指向 tuoen 的 pip 根  out=PYTHONUSERBASE=…\tuoen\globals\pip PIP_USER=1
[ok]   子进程里 PIP_USER = 1  out=PYTHONUSERBASE=…\tuoen\globals\pip PIP_USER=1
[ok]   shell 里的 pip list --user 退出 0  exit=0 err=
[ok]   shell 里 pip list --user 看到的包 = globals list 里 tuoen 侧的 pip 包  child=pypinyin tuoen=pypinyin
[ok]   跑完 shell 之后 HKCU\Environment 逐字未变  values=14
[ok]   注册表里仍然没有 NPM_CONFIG_PREFIX
[ok]   当前这个 shell 自己也没被污染
```

最后那条 `pip list --user` 的对照是这次**新增**的（票据 §6 要求的另一半）：环境变量只能
证明"我们设了"，证明不了"pip 真的用了它"。本机实测这条断言**会红**：不设重定向时
`pip list --user --format=json` 印 `[]`，设上之后印 tuoen 根里的 `pypinyin`。

### 3.4 §5 真装 + §6 幂等

```
[ok]   计划里有一条 globals-prefix-moved 的意图  codes=globals-prefix-moved
[ok]   人类输出逐字说出"不在机器自己的 prefix"  含前缀=True 含不在=True
[ok]   restore --apply --offline 退出 0  exit=0
[ok]   磁盘上真的有 …\tuoen\globals\npm\v24.19.0\node_modules\pnpm
[ok]   装进去的是精确版本 11.21.0  version=11.21.0
[ok]   第二次是 no-change（幂等判据只比 tuoen 侧）  status=no-change
[ok]   tuoen 来源里有且只有一个 pnpm  rows=1
[ok]   机器来源的行数一个字都没变（决策 167）  before=10 after=10
```

### 3.5 §7 / §7b shim：**装好的包要敲得出来**

npm 侧（离线就能验）：

```
[note] shim 目录里有 5 个文件：pn.exe,pnpm.exe,pnpx.exe,pnx.exe,pypinyin.exe
[ok]   从装进去的 package.json 读到了 pnpm 的 bin 名（票据实测 4 个）  bins=pnpm,pnpx,pn,pnx
[ok]   shim 的 `pnpm --version` 与真身逐字相同  shim=11.21.0 real=11.21.0
[ok]   shim 报的版本就是快照里那个精确版本 11.21.0  shim=11.21.0
[ok]   shim 目录里没有任何 .cmd / .ps1（铁律：绝不发这两个）  bad=
```

pip 侧（`-WithNetwork`）：

```
[ok]   pip 的 payload 在 `<根>\<PythonXY>\Scripts` 里（版本段从 pip --version 反推）  tag=Python312 path=…\tuoen\globals\pip\Python312\Scripts\pypinyin.exe
[ok]   载荷里报出 pypinyin 发出来的 shim 名（restore 自己发的）  shims=pypinyin
[ok]   载荷里 shimCommands 是"查过"的那一态（不是 null）  shimCommands=pn,pnpm,pnpx,pnx,pypinyin
[ok]   restore 自己发出了 `<shims>\pypinyin.exe`（不是 `shim add` 发的）  path=…\tuoen\shims\pypinyin.exe
[ok]   pip shim 的 `--version` 与真身逐字相同  shim=pypinyin 0.55.0 payload=pypinyin 0.55.0
[ok]   pip 侧也没发 `.cmd` / `.ps1`  bad=
```

### 3.6 §8 红线（12/12）与 §9 实测耗时

```
[ok]   产品源码里没有调用 setx  hits=0
[ok]   本脚本里没有会执行 setx 的语句  hits=0
[ok]   反向验证：判据对一行真的 setx 调用会命中
[ok]   HKCU\Environment 跑前跑后逐字相同  before=14 after=14
[ok]   用户级 Path 逐字未变  sha16=a12fc582e90513a3
[ok]   机器级 Path 逐字未变  sha16=d0b5a0db7c9e2a6b
[ok]   NVM_HOME / NVM_SYMLINK 两个作用域逐字未变
[ok]   nvm4w 的 settings.txt 哈希未变  sha=02f3156cdf563d43b621d37c36d36bf1adf74f915ab98f241982abcc68e00555
[ok]   机器自己的 npm prefix 没被碰  prefix=C:\nvm4w\nodejs
```

同一个包在**两个来源**上各出现一次（`source = "machine"` 与 `source = "tuoen"`）时，
上面的红线证明"我们只写了自己那一侧"：机器侧 npm 的 7 行一个字都没变。

耗时（[`L2-timings.txt`](L2-timings.txt)）：`globals list` 3634 ms · `npm ls -g` 885 ms ·
`capture globals` 3293 ms · `restore plan globals` 4819 ms · `restore apply globals` 7088 ms ·
`shim list` 17 ms · `restore apply pip (network)` 5424 ms。

## 4. 我错了（五条，产品都是对的）

1. **`-SkipBuild` 的 mtime 守卫红了一次，**报的是"release 二进制不比最新源码旧"失败
   （`exe=21:15:08 最新源码=21:19:07 (shims.rs)`）。那不是产品错，也不是守卫错：我刚跑完一轮
   变异测试，**还原文件时刷新了 mtime**（哈希与备份逐字相同）——守卫按它自己的规则是对的，
   该做的是重新构建。这正是 #21 教训的第二次现场：**哈希相同不能证明"跑的是同一份代码"**。
2. **我假设用户级 pip 的脚本目录是 `<根>\Scripts`** —— 实际是 `<根>\Python312\Scripts`
   （`site-packages` 也在 `<根>\Python312\` 下面、**没有** `Lib` 那一层）。产品对、我的期望错。
   修法：版本段从 `pip --version` **自己反推**，不写死 `Python312`（写死会让这条断言在换了
   Python 版本之后红在一个产品做对了的地方）。
3. **我用 `tuoen shim add pypinyin` 当"发 shim"的手段 —— 概念就错了。**
   `shim add` 认的是**编目里的工具**（node/temurin/oracle-jdk/msvc），拿**包名**去问它得到的是：
   ```
   {"ok":false,"error":{"code":"unknown-tool","message":"目录里没有工具 `pypinyin`；已知：node, temurin, oracle-jdk, msvc"}}
   ```
   正确的手段是 **`restore` 自己**（#25 的能力）。这一条的价值超过修它本身：它说明在 #25 之前，
   "经 `tuoen shell` 装进 tuoen 根的包**敲不出来**"是**必然**的 —— 那正是 #25 要补的洞。
4. **我新写的断言用了 `$pipBad.Name`（空集合上取属性）**，StrictMode 下脚本当场**消失**：
   ```
   acceptance-L2-27.ps1: 在此对象上找不到属性"Name"。请验证该属性是否存在。
   ```
   连 SUMMARY 都没打（"脚本消失"与"产品错了"看起来一模一样 —— #16 家族的第 5 次）。
   修法用脚本里**已有**的写法：`@($x | ForEach-Object { $_.Name })`。
5. **第一版漏了票据本来就写着的"清理包在 `try/finally` 里"。** 补的时候没有只用眼睛确认，
   而是**把第 4 条那个会抛异常的形态改回去跑了一遍**：`exit=1`、报同一句属性错误，而
   **工件目录不复存在**（`CLEANED …`）—— 一个 `finally` 在"中途抛异常"这条路径上真的会跑，
   这是证明，不是断言。

## 5. 这一票改了什么（只有 `scripts/` 与 `docs/`）

* `scripts/acceptance-L2-27.ps1`
  * **§7b 重写**：不再用 `shim add`，改成断言 **`restore` 自己**发出来的 shim（载荷里的
    `shims` 数组 + `shimCommands` 那一态 + 盘上的 `.exe` + 与真身逐字对照），并且**可重复**
    （先删 `pypinyin*` 产物再装，理由见下）；新增"要删的路径必须在 tuoen 根下面"的守卫。
  * **§1 新增** `where python` 的第一条（连"它是什么"一起记）。
  * **§4 新增** `pip list --user --format=json` 与报告里 tuoen 侧 pip 行的对照。
  * **整个验收体包进 `try`、收尾进 `finally`**，新增 `-KeepArtifacts`（默认跑完删工件目录）；
    头注释修成它自己的名字（原来那段 `.EXAMPLE` 还写着 `acceptance-L1-18.ps1`、`.DESCRIPTION`
    还写着"#18"），并补上 `-WithNetwork` 的说明。
* `docs/acceptance/`：本文件 + 三个逐字输出 + 人类面 + shell 输出 + 耗时。

**§7b 为什么要"先删再装"**：`already-present` 的包**故意不补 shim**（决策 212），所以第二轮
验收的计划是 `no-change`、发 shim 那一步根本不会走 —— 不删的话，新断言会红在一个**产品做对了**
的地方。

## 6. 发现的问题

* **一条产品缺口（已开 [#35](https://github.com/Muelsysel/tuoen/issues/35)）**：`restore` 只给"这一趟真的装上了"的包发 shim；
  `already-present` 与 `no-change` 一律不补（决策 212）。用户能撞上的形态是：
  第一次安装时某个 shim 没发成（比如当时 store 里没有那个精确版本的 node），之后
  再跑 `restore` 也**永远不会**补上它 —— 而报告里不会说这件事。
* **两条是我的脚本错**（§4 第 2、3、4 条），产品没错：`shim add` 的 `unknown-tool` 是**正确**的
  错误码（包名不是工具名）。
* 没有发现别的产品 bug。

## 7. 诚实未覆盖

1. **只在一台机器上跑过**（作者本机），且这台机器的 PATH 已经很长（1781 字符 / 48 条）、
   机器级 `Path` 是 `REG_SZ` —— 别的形态（`REG_EXPAND_SZ`、8191 悬崖附近）没有真机覆盖。
2. **§7b 默认不跑**（`-WithNetwork` 才跑）：pip 的联网安装没法离线验。默认跑的 SUMMARY 里
   永久留着 **1 条 skip**，这是**刻意的诚实不对称**，不是待修的 bug。
3. **pip 侧只覆盖了一个包**：`pypinyin`（它的 `RECORD` 里有一条 `Scripts/pypinyin.exe`）。
   本机第三个 pip 包 `pypdf` 的 `RECORD` 里**没有** `Scripts` 条目 ⇒ 它拿不到命令名、
   不发 shim —— 那条"没有 Scripts 条目"的路只被单元测试覆盖过。
4. **作用域包的 npm bin 没走真机**：`@deepseek-ai/dsh` → `dsh` 这条路（`bin` 值是
   `./lib/…`）只有单元测试；真机上装进 tuoen 根的是 `pnpm`。
5. **脚本不负责装 node**：§5/§7b 依赖 store 里已经有那个精确版本的 node
   （`nodes\versions\<削过的版本>\node.exe`）；没有时产品会报 `node-missing`（那是 #25 的
   单元测试覆盖的路径），而本票的真机路径从"node 已经在"开始。
6. **`%TEMP%` 的工件默认在收尾时删掉**，想留证据要用 `-KeepArtifacts`（本文件引用的
   `L2-globals-list-human.txt` / `L2-shell-outputs.txt` / `L2-timings.txt` 就是这么拿到的）。
7. **`hkcu-environment.json`（跑前跑后的整个 `HKCU\Environment` 快照）不进仓库**：
   它按设计包含**所有** 14 个值，其中就有 `capture` 刻意跳过的 `ARK_API_KEY` 的值。
   它只留在 `%TEMP%` 的工件目录里。

## 8. 跑完之后这台机器上多了什么（唯一的改动）

本票**唯一**写下的东西是 tuoen 自己的根与 shim 目录：

```
%LOCALAPPDATA%\tuoen\globals\npm\v24.19.0\node_modules\pnpm   （§5，离线装的）
%LOCALAPPDATA%\tuoen\globals\pip\Python312\{site-packages\pypinyin*,Scripts\pypinyin.exe}   （§7b，联网装的）
%LOCALAPPDATA%\tuoen\shims\{pn,pnpm,pnpx,pnx,pypinyin}.exe   （#25 的自动发布）
```

除此之外：`HKCU\Environment` 14→14 逐字未变、两个作用域的 `Path` 哈希未变、
nvm4w 的 junction 与 `settings.txt` 未变、机器自己的 npm prefix 仍是 `C:\nvm4w\nodejs`、
产品源码里 0 处调用 `setx`。**没有往机器自己的 prefix 里装任何东西。**

## 9. 门禁与复现

这一票**没动 `crates/**`**，所以门禁分两半：

* **"没动"是可验证的**：`git status --porcelain` 里只有 `scripts/acceptance-L2-27.ps1` 与
  `docs/acceptance/L2-*`；`-SkipBuild` 时脚本自己**断言** release 二进制不比最新源码旧
  （而源码的 mtime 只在 `crates/**` 变化时才动）。
* **上一次全量门禁**（#25 收尾、同一个 `crates/**`）：`cargo fmt --all --check` 0 ·
  `cargo check --workspace --all-targets` 0 · **38 target / 1436 passed / 0 failed** ·
  `clippy --workspace --all-targets -- -D warnings` 0。

复现（三条命令 + 三个 SUMMARY）：

```
pwsh -NoProfile -File scripts/acceptance-L2-27.ps1                 # 104/0/1  verdict=PASS  exit 0
pwsh -NoProfile -File scripts/acceptance-L2-27.ps1 -WithNetwork    # 114/0/0  verdict=PASS  exit 0
pwsh -NoProfile -File scripts/acceptance-L2-27.ps1 -SelfTest       # 104/1/1  verdict=FAIL  exit 1
```
