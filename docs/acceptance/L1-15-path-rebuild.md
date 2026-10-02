# L1-15 验收：`path diff` / `path apply`（PATH 逐条 diff + 重建 + 选择性应用）

票据的立场是"本机 `PATH` 已经是坏的（12 组重复、7 条失效、1 个拼错 3 次的目录、12 条硬编码用户名），
**原样搬运会把旧病一起移植**"，所以这一票做的不是复制，而是**重建 + 逐条 diff + 选择性应用**。

它的失效形态一个都不是崩溃：

- **顺手重排**：用户只想 `--only add`，结果整条 `PATH` 按目标顺序被重排 —— 重排就是改优先级；
- **预览与执行不一致**：用户照着 `--dry-run` 的结论点了"确认"，写下去的却是另一份；
- **写进去就静默截断**：超过 8191 之后 `cmd.exe` **整条忽略** `PATH`；
- **"修好了"其实没修**：一条同时失效又硬编码了旧用户名的条目被归成"丢掉"而不是"重写"，
  或者归成"重写"却没有旧名可换 —— 两种都是"看起来在工作"。

- 验收脚本：`scripts/acceptance-L1-15.ps1`（`-SelfTest` 必须 exit 1）
- 机器工件：`docs/acceptance/L1-15-path-rebuild-run.txt`
- 设计决策：`docs/DESIGN.md` §1.17（126–140 是设计期、141–146 是实现与复核期）

## 1 逐条验收

### 1.1 票据「验收标准」四条

| 验收标准 | 证据 |
| --- | --- |
| `cargo test --workspace` 全绿，clippy `-D warnings` 干净 | §6 |
| **在作者本机上真实跑一次 `tuoen path diff`**，把完整 diff 贴进 issue 评论，并与取证报告对照 | §3.1、§2.2 |
| **证明 `--dry-run` 未产生副作用**（前后各贴一次 `HKCU\Environment\Path` 的字符数与哈希） | §3.2 |
| 贴出"选择性应用"的一次真实演示（只应用 `add` 类） | §3.3 |

### 1.2 票据「必须实现的」六项

| 票据要求 | 落点 |
| --- | --- |
| **逐条 diff，六类**（`keep`/`add`/`remove`/`move`/`fix`/`case-only`），每条带**理由**与来源快照的 `owner`/`reparse`/`has_username` | `crates/core/src/pathdiff.rs` 的 `diff()`；`PathDiffRow{id,class,reason,also,target,local,duplicate_of,rewrite_to,suggestion}`；`SideRow` 逐字带那三项事实；判据优先级见决策 127（`class == reason.class()` 是不变量，脚本全量核对） |
| **重建**：去重保留首次、失效条目归 `fix` 不静默删、**不做拼写自动纠错但必须报告疑似**、重写用户名依赖**并逐条报告**、保留"机器级在前/用户级在后"、重建后查 8191 悬崖 | `rebuild()`（决策 131/132 的稳定双源合并 + 放置规则）；`suspected_typo()`（编辑距离 ≤ 2 + 父目录真的存在那个名字）；`Rewrite` 逐条；`Rebuild.budget` |
| **选择性应用**：按类别选、按条目选（`--pick <id>`）、**预览后放弃不留副作用** | `Selection{classes,picks}`；`--only`/`--pick`；`--dry-run` 与真写共用同一套 `diff`+`rebuild`（决策 139） |
| **写入实现**：绝不 `setx`、含 `%` 才 `REG_EXPAND_SZ`、不展开读、写后广播、**机器级不静默提权** | 复用 L0 的 `apply`（整值写 + 广播）+ `write_type_for`；`apply_rewrite` 在类型层面拒绝非用户级；机器级改动进 `requiresElevation` |
| **`--dry-run` 走同一套代码路径** | 契约测试断言两次 `--dry-run --json` 的 `scopes` 逐字节相同；真机 §3.2 |
| **可见性说明**：只有 Explorer 及其之后新起的子进程能拿到，我们自己的进程也拿不到，并明确"请重启终端是平台限制" | `path_diff_view.rs` 的人类输出固定两段（决策 138）；`--json` 里是 `visibility: "restart-required"` |

### 1.3 票据「必须有的用例」逐条落点

| 票据点名 | 落点（core 71 条 + platform 35 条 + CLI 契约测试） |
| --- | --- |
| 六种 diff 类别各一个 | `pathdiff.rs` 的六类用例各一条，逐条断言 `class`/`reason`/`id` |
| **仅大小写差异必须判为 `case-only`**，不是"一个 add + 一个 remove" | `counts` 断言：恰好一行 `case-only`、`add`/`remove` 均为 0 |
| 去重保留**首次**位置（`A;B;A` → 保留第一个 A） | `A;B;A` → 第一行 `keep`、第三行 `fix`/`duplicate` 且 `duplicate_of == "user:0"` |
| 空条目：`;;`、开头、结尾各一个 | 三处都归 `fix`/`empty-segment`，且**不与任何目录配对** |
| 8191 边界：`1719`/`8190`/`8191`/`8192`，断言重建后的检查行为 | 四档用例按 `PathBudget::of` 的真实阈值断言（`ok`/`critical`/`critical`/`exceeded`）—— 阈值是 `>8191` / `≥7371.9` / `≥6143.25`，所以 `8190` 与 `8191` **都已经在 `critical` 档**（票据期我猜的"落在 warning"是错的，用例里逐字写了这条更正） |
| 用户名重写：机器级与用户级各一条，且断言**报告了重写清单** | `rewrites` 逐条 + `rewrite_to`；真机演示见 §3.4（本机 12 条全是当前用户 → 0 条重写，所以用**合成的旧用户名快照**演示） |
| 拼写错误识别：`C:\Software\tool` vs 存在的 `C:\Software\tools` → 报告为疑似 | `suspected_typo` 的正反用例；真机 §2.3（脚本自己核对每条建议**真的存在**且父目录相同） |
| **选择性应用**：只选 `add` → 断言 `remove` 类条目**未被改动** | core 用例断言"连位置都没动"；真机 §3.3（`--only fix` 时 remove 类条目下标逐条不变） |
| **放弃预览 → 零副作用**（断言假后端未被写） | platform：`before == after` 时 `wrote == false` **且广播闭包一次都没被调用**；真机 §3.2（哈希逐字相同） |
| `--dry-run` 与真实执行的 diff **逐条相同** | CLI 契约测试（同一 shell 两次 `--json` 逐字节相同）+ 决策 139 |

### 1.4 硬性约束

- **core/platform 的测试不读写真实注册表**：两侧共 106 条用例全部走 `FakeFileSystem`/`FakeRegistry`/
  `FakeMachine`/自写的最小 `EnvBlock` 假后端，**没有一条读真机**（`cargo test -p tuoen-platform` 的
  假后端里连"写入次数"这个量都不存在，所以"没写"只能由"写完读回来还是旧值"间接证明 —— 见 §5）。
- **测试不访问网络**：本票没有任何 `Transport`/`curl` 代码路径。
- **测试不得改开发机的 `PATH`**：CLI 契约测试**只读**真实注册表（决策 146 —— 这一层没有注入点，
  而 AGENTS.md 禁止给出厂二进制留后门开关），每条触及注册表的用例结尾都断言两个作用域的 `Path`
  **逐字节相同**；**写**只发生在验收脚本的 §6（本节 §3.3）那一次真实演示里 ——
  它自己备份、**写入之前先自证还原路径能用**、写完立刻还原、并用哈希证明一个字节都没留下
  （这条"先自证还原"的规矩来自 §4 第 5 条那次真机事故）。

### 1.5 明确不做（票据点名）

- **不自动清理失效条目**：它们归 `fix`，删不删由用户选（`--only fix`）。
- **不做拼写自动纠错**：只报 `suggestion`，而且脚本会核对"建议的那个目录真的存在"。
- **不接管第三方版本管理器的条目**：`C:\nvm4w\nodejs` 只作为 `owner = third-party` 参与 diff，
  重建不会改写它（`owner` 的判据只有三条，见 `collect/path.rs` 的模块文档）。
- **不实现完整的 `restore`**：那是 #16；本票的出口是 `path diff` 与 `path apply`。

## 2 真机事实与独立重算

### 2.1 分母先说清楚

验收脚本**自己**从注册表（`DoNotExpandEnvironmentNames`，**不展开**）与 `tuoen.d/path.toml`
（用 Python 的 `tomllib` 独立解析，**不经过 tuoen 的任何代码**）各数一遍：

```text
快照：machine 33 条 / user 16 条；budget 773 + 1007 = 1781
快照的 machine 条目数 = 脚本自己数出来的   33 / 33
快照的 user 条目数   = 脚本自己数出来的   16 / 16
当前用户名（脚本自己从进程环境取）：Muelsyse
```

`HKCU\Environment\Path` 是 `REG_SZ`、**773** 字符；`HKLM\…\Path` 是 `REG_SZ`、**1007** 字符；
"新终端口径"的合并长度是 `1007 + 1 + 773 = 1781`（那一个字符是分隔符）。

### 2.2 六个类：脚本自己算一遍，再与产品对照

判据（决策 127，优先级：存在性 → 健康 → 位置 → 大小写 → 相同）：

```text
脚本自己数：keep=25 add=1 remove=0 move=0 fix=24 case-only=0（共 50 条 = 49 条真条目 + 1 条演示用追加）
fix 明细：dangling=5  duplicate=18  empty-segment=1   （username-hardcoded=0，见 §2.4）
```

### 2.3 与取证报告的对照（票据要求逐条说明）

| 取证报告（票据期） | 今天独立实测 | 说明 |
| --- | --- | --- |
| 10 组重复 | **12 组 / 18 条富余** | 机器状态变了（用户后来又装了 Python 3.11/3.12 等）。票据期的 `725/978` 也是同一批过期数字 |
| 7 条失效 | **7 条**（机器级 3 + 用户级 4） | 数字一致 |
| 11 条用户名依赖，**其中 2 条在机器级** | **12 条**（机器级 **2** + 用户级 10） | 机器级 2 条逐字一致；总数 11 → 12 |
| `C:\Software\tool` 拼写错误 | **逐字复现**：`C:\Software\tool` 在 user 出现 1 次、machine 出现 **2** 次（共 3 次，从未生效），而 `C:\Software\tools` **真的存在** | `C:\Software` 下的目录是 `App` / `Drive` / `tools` |

**两个分母必须分开说**：`dup_index > 0` 数的是**富余条数**（18），而"重复的组数"是 **12**；
`fix` 的 24 条是**归并之后**的数（优先级让一条既重复又失效的条目只算一次），
而原始失效数是 7（其中 2 条同时是重复：`C:\Software\tool` 出现 3 次，
`machine:8` 是主条目、`machine:28` 与 `user:11` 被 `duplicate` 抢先）。
把 18 与"10 组"、把 24 与"7 条失效"直接对照，得到的都是假矛盾。

### 2.4 用户名依赖：12 条事实、0 条动作

本机 12 条硬编码用户名的条目**全部是当前用户**（`Muelsyse`），所以**没有旧名可换**，
按决策 141 它们不进 `fix`（`fix` 的含义是"这一类的 fix 会做什么"，而这一条没有可做的事），
而是落在 `dangling`（3 条）/ `duplicate`（2 条）/ `keep`（7 条）里，
**可移植性这个事实留在 `SideRow.has_username` 与 `also` 里**，脚本另外单独统计：

```text
本机独立计数：dup_index>0 18 条 / 失效 7 条 / 硬编码用户名 12 条 / 空条目 1 条
用户名依赖：机器级 2 条 + 用户级 10 条
```

跨机的形状由 §3.4 的**合成旧用户名快照**演示（`C:\Users\tuoen-old-user\bin` →
`C:\Users\Muelsyse\bin`，且插入的值就是重写后的那个）。

## 3 真机演示

机器工件：`docs/acceptance/L1-15-path-rebuild-run.txt`（`checks_passed=115 checks_failed=0 checks_skipped=0
verdict=PASS`，`real_write_restored=True`）与 `docs/acceptance/L1-15-path-diff-human.txt`（作者本机真实快照的完整人类 diff）。

### 3.1 完整 diff（作者本机，`path diff` 人类输出）

`tuoen capture --only path` 现场产出快照，再 `tuoen path diff <path.toml>` → exit 0，66 行，
**`keep 25 · add 0 · remove 0 · move 0 · fix 24 · case-only 0`**（与 §2.2 脚本自己算的逐个数字相同）。
完整输出在 `L1-15-path-diff-human.txt`；下面是逐字摘录（每类各取几条，以及那三条"只有真机能给"的行）：

```
当前用户名：Muelsyse（来自进程环境 USERPROFILE）
  （同一份快照在不同的当前用户名下**类会不同** —— 用户名依赖的行只有在真的有旧名可换时才归 `fix`，其余按它别的性质归类。）

逐条 49 行（本机侧的行 + 只有目标侧的行）：
  keep 25 · add 0 · remove 0 · move 0 · fix 24 · case-only 0

  [machine:1] fix dangling C:\Program Files (x86)\Common Files\Oracle\Java\javapath —— 疑似错写 → C:\Program Files (x86)\Common Files\Oracle\Java\java8path
  [machine:7] keep identical C:\Users\Muelsyse\AppData\Local\Microsoft\WindowsApps（还同时是：username-hardcoded）
  [machine:8] fix dangling C:\Software\tool —— 疑似错写 → C:\Software\tools
  [machine:9] fix duplicate C:\WINDOWS —— 与 machine:3 重复
  [machine:21] fix empty-segment
  [user:13] fix duplicate C:\nvm4w\nodejs —— 与 machine:18 重复
  [user:15] keep identical C:\Dev\IDE\VScode\Microsoft VS Code\bin

硬编码了用户名的条目：12 条（机器级 2 条 · 用户级 10 条）—— 换账号名之后它们会静默失效。
  （按 `local.hasUsername` 数的，**不是**按 `fix` 类数的：名字就是当前用户名时没有旧名可换，那些行按其余性质归类，这条事实留在 `also` 里。）

用户名重写：没有（没有「旧名可换」的条目）。

拼写疑似：5 条 —— **只报告，永不自动纠错**（改错一个目录名比不改更危险）。

这只是报告，**什么都没写** —— 没有写注册表、没有落盘、没有广播。
```

三条只有真机能给的行，逐条说明：

- `[machine:9] fix duplicate … 与 machine:3 重复`：本机机器级里 `C:\WINDOWS` / `C:\WINDOWS\system32` /
  `…\Wbem` / `…\WindowsPowerShell\v1.0` / `…\OpenSSH` 各出现 **两次**（小写与大写写法各一次，
  决策 128 的 `case-only` 之所以单列就是为了不把它们报成 add+remove）—— 去重保留**首次**出现的那条
  （`machine:3`），后面那条归 `fix`/`duplicate` 并**指名**它跟谁重复。
- `[machine:21] fix empty-segment`：真机上真的有一个空条目（`;;`），它不与任何目录配对（决策 130）。
- `[user:13] fix duplicate C:\nvm4w\nodejs —— 与 machine:18 重复`：**跨作用域**的重复 ——
  nvm4w 的安装器把 `C:\nvm4w\nodejs` 同时写进了机器级与用户级，而进程 `PATH` 里两个都在，
  所以这个名字在用户级那一份是**永远轮不到**的。判据按**同一作用域内**去重，`duplicate_of`
  指向机器级那条 —— 报告说的是事实，删不删由用户选。

### 3.2 `--dry-run` 零副作用（前后各一次字符数与哈希）

`path apply <目标快照> --only add --dry-run`（真机，目标快照 = 真快照 + 1 条 add 行）→ exit 0，
随后脚本自己读回两个作用域（类型 + **不展开**原文）：

| 口径 | 跑之前 | 跑之后 |
| --- | --- | --- |
| `HKCU\Environment\Path` 字符数 / 类型 | 773 / `REG_SZ` | 773 / `REG_SZ` |
| `HKCU\Environment\Path` sha16 | `a12fc582e90513a3` | `a12fc582e90513a3` |
| `HKLM\…\Path` 原文 | 1007 字符 | **逐字未变** |
| `%LOCALAPPDATA%\tuoen` 清单（含 store 两层） | — | **逐项相同** |

同一段里还断言了 `--json` 的 `dryRun=true` / `wrote=false`、`afterRaw` 逐字等于脚本自己算的
"本机列表 + 追加末尾"（808 字符）、以及机器级**没有** `applied` 行（不静默提权）。

### 3.3 选择性应用的一次真实演示（只应用 `add` 类）

这是全票唯一一次真的写 `HKCU\Environment\Path`，包在 `try/finally` 里，且**写入之前先自证还原路径能用**
（§4 第 5 条）。逐字证据：

```
  备份（跑之前）：773 字符 / String / sha16 a12fc582e90513a3 → …\_l115_hkcu_backup_script.txt
  [ok]   还原机制自检：原样写回一次后哈希与类型逐字相同（§6 的还原路径真的能写）  a12fc582e90513a3 → a12fc582e90513a3 / String
  [ok]   path apply --only add 退出 0  exit=0
  [ok]   真写：wrote = true  True
  [ok]   写进去的值 = 脚本自己算的期望值（逐字）  808 字符 / 期望 808 字符
  [ok]   写进去的类型仍是 sz（值里没有 %）  String
  [ok]   新值比旧值正好长 1 + 目录长度  773 → 808
  [ok]   新终端里 where mvn 找到了 mvn.cmd  C:\Dev\Tool\apache-maven-3.9.5\bin\mvn | C:\Dev\Tool\apache-maven-3.9.5\bin\mvn.cmd
  [ok]   写完之后再 diff：add 类归零  0 条
  [ok]   写完之后再 diff：keep 类增加了（多了一条）  25 → 26
  [ok]   还原后 HKCU Path 哈希与跑之前逐字相同  a12fc582e90513a3 → a12fc582e90513a3
  [ok]   还原后 HKCU Path 字符数与类型都相同  773 / String
  [ok]   还原后新终端里 where mvn 又找不到了  INFO: Could not find files for the given pattern(s).
```

值得点名的三件事：

1. **写进去的值是脚本自己算的**，不是产品说它写了什么：`afterRaw` 与
   `($localUser | % { $_.Raw.Trim() }) -join ';'` + `;` + 演示目录 **逐字相同**（808 字符）。
2. **生效只能在重建出来的环境里验**（AGENTS.md 规矩 6）：`where mvn` 是在
   `scripts/fresh-terminal.ps1` 造出来的"新终端"里跑的，第一个命中就是
   `C:\Dev\Tool\apache-maven-3.9.5\bin\mvn.cmd` —— 而那个目录在跑之前**不在任何一个作用域里**。
3. **"没留下一个字节"是哈希级证明**，不是"我们还原了"：还原后 `HKCU` 的 sha16 与跑之前逐字相同，
   并且**我在脚本之外**拿第一次跑之前写下的那份手工备份又比了一次（773 字符 / `String` / 逐字相同）。

### 3.4 用户名重写（本机 12 条全是当前用户 → 用合成快照演示）

真机上 12 条硬编码用户名**全部是当前用户名**（`Muelsyse`），所以 `rewrites` 是空的 ——
这不是"功能没实现"，而是决策 141：**没有旧名可换的行不归 `username-hardcoded`**，
它们按其余性质归类，这条事实留在 `also` 里（§2.4）。为了真的演示重写，脚本把目标快照里
一条用户级条目的用户名换成 `tuoen-old-user`：

```
  [ok]   旧用户名那一行被认出来了（类 add）  1 条
  [ok]   rewriteTo = 当前用户名的那条路径  C:\Users\Muelsyse\bin
  [ok]   rewrites 逐条报告了这次重写  1 条：C:\Users\tuoen-old-user\bin→C:\Users\Muelsyse\bin
  [ok]   插入的值是**重写后**的路径（不是旧用户名那条）
  [ok]   新列表里没有旧用户名那条
```

即：`rewrite_to` 与 `rewrites` 都在报告里，而**只有真的选中 `fix`/`add` 时**才会把重写后的值写下去。

### 3.5 票据原话那一条：只选 `add` 时 `remove` 类不许被删（§4d）

本机真快照里 `remove` 是 **0 条**（目标就是本机），所以这条判据在真机上是**空真**。
脚本因此造了一份"目标里少了最后 3 条用户级条目"的快照（只删 `[[entry]]` 块、其余文本逐字保留）：

```
  [ok]   这份快照真的产生了 remove 类（本机真快照里是 0 条 → 空真）  3 条
  [ok]   只选 add：applied 里恰好一条（没有顺手删 remove）  1 条
  [ok]   只选 add：重建结果 = 本机全部条目 + 追加的那条（逐字）  产品 808 字符 / 脚本 808 字符
  [ok]   只选 add：3 条 remove 类的值一条都没丢
```

—— 重建结果是**本机列表 + 追加的那条**，那 3 条 `remove` 类条目**连位置都没动**（决策 131）。

## 4 验收期抓出来的问题

按"谁错了"分类。前四条是**验收脚本自己造的假结论**（它们的共同点：看起来像产品错了），
第五条是**最严重的一条**，第六、七条是复核期抓出来的产品瑕疵。

1. **`@($null).Count` 是 1，不是 0。** 第一版每条成功断言都直接读 `--json` 的**信封**
   （`{schemaVersion, command, ok, data:{…}}`），于是 `Prop $j 'rows'` 返回 `$null`，
   `@($null).Count` 数出 **1** —— 五条断言同时报"产品 1 条 / 脚本 50 条"，看起来像产品只输出了一行。
   修法：加 `Get-Payload`（统一剥 `data` 信封）与 `Arr`（先滤 `$null` 再 `@()`），
   并且 `$x = Arr …` 一律写成 `$x = @(Arr …)`（**函数返回空数组时调用方拿到的是 `$null`**）。
2. **"位置不变"是子序列，不是同下标。** `--only fix` 会删掉重复与空条目（16 → 7），后面的条目
   自然往前挪；我按"同下标逐字相同"比，于是在**做对了**的时候报红
   （`第 15 段：'C:\Dev\IDE\VScode\Microsoft VS Code\bin' → '<越界>'`）。
   同理，拿"after 的条数"去比"本机条数 − remove 类条数"是把"该删的"和"不该删的"混成一个数。
   修法：比**子序列** —— 没被 `applied` 碰过的条目必须按原顺序逐字出现。
3. **机械替换会改写它自己要引入的那个函数。** 用正则把 `@(Prop X 'y')` 批量换成 `Arr X 'y'` 时，
   新写的 `Arr` 定义体里那一行 `@(Prop $Obj $Name | …)` 也被同一条正则命中，`Arr` 于是变成
   "调用自己"，报错是 **"由于调用深度溢出，脚本失败"**。教训：批量替换之后要**回头读一遍被替换的
   区域**，而不是只看"替换了 22 处"。
4. **双引号 here-string 里 `` `a `` 是 BEL。** 生成 `[[entry]]` 块时注释里的 "add" 被写成 `\x07dd`，
   `tomllib` 报 `Found invalid character '\x07' (at line 870, column 34)`。修法：**单引号
   here-string + 占位符替换**（这条在 #12/#13 已经记过一次，这是第二次踩）。
5. **【最严重】还原路径从没被验证过，于是"写进去了、还原不了"。**
   第一次真机跑：`--only add` 真的写成功（808 字符，`wrote=true`，新终端里 `mvn` 找得到），
   而 `finally` 里的还原抛异常：

   ```
   acceptance-L1-15.ps1: 调用"SetValue"并传入"3"个参数时发生异常："Cannot write to the registry key."
   ```

   根因：`Set-PathValue` 用的是 `(Get-Item 'HKCU:\Environment').SetValue(...)`，而 PowerShell 的
   注册表 provider 返回的是**只读**句柄，`SetValue` 必然抛。于是 `HKCU\Environment\Path`
   被留在 808 字符的改动值上 —— 直到我按**跑之前手工写下的**备份（`%TEMP%\_l115_hkcu_backup.txt`，
   773 字符 / `REG_SZ` / sha16 `a12fc582e90513a3`）用**可写子键句柄**写回并逐字核对哈希。
   原始输出保存在 `docs/acceptance/L1-15-path-rebuild-restore-incident.txt`。

   修法两条，**顺序很重要**：
   1. `Set-PathValue` 改用 `[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)`；
   2. §6 在**真实写入之前**先做一次"还原机制自检"——把当前值**原样写回一次**并断言哈希与类型未变。
      **一条不能失败的还原不是还原**：还原路径必须先自证能用，才允许写入路径跑。
      另外 `finally` 里现在会**当场**复核哈希，不一致就红字喊出来并指出备份在哪。

6. **复核期（C 自查）**：`path apply` 人类输出对用户级恒印 `**会写**`，而 `applied` 为空、
   `wrote=false` 时那句话是**错的**（下面 `print_visibility` 那句"计划与现状一致"只盖住了一半）。
   修法：人类输出的标签改用 `PathPlan::will_write()`（CLI 不自己重算 —— 重算就是第二份事实），
   bool 只走参数、**`--json` 一个键都没动**；契约用例
   `the_user_scope_line_says_whether_it_will_really_be_written` 钉住它，并做了变异真红。
   验收脚本新增 §4e 独立复核两个方向：no-op 计划**不许**出现 `**会写**`，`--only fix` 那一档仍须印 `**会写**`。
7. **复核期（我裁决）**：决策 140 的字面是"`exceeded` → 拒绝**写入**"，而 C 第一版把 `--dry-run`
   一起拒了。**这是错的**：`path apply` 是**唯一**能产出"重建后的完整列表"的命令，
   若预览也被拿走，一个 `PATH` 已经 9000 字符的用户连"要删哪几条才能降下来"都无从知道。
   改成决策 147：`--dry-run` + `exceeded` → **退出 0 + 完整计划**，并额外说一句"真写会被拒绝"；
   真写 + `exceeded` → 退出 1 + `too-long`，**载荷里仍带完整计划**。两条用例都做了变异真红。

## 5 诚实未覆盖

- **只有一台机器**，而且只有这一台机器的 `PATH` 形状（`REG_SZ`、无 `%` 变量、无引号条目）。
  `expanded` 的"`expand-sz` 才展开"那一支、`case-only`（本机 0 条）、`move`（本机 0 条）
  在真机上**都没有真实样本**，只有夹具样本。
- **`--pick` 的真机演示只有一条 add**：多选的组合、以及 `--pick` 一条 `keep`（它会进
  `noOpClasses`）只有契约测试覆盖。
- **CLI 层的测试读真实注册表**（决策 146）：这是票据"测试不得读写真实 `HKCU\Environment`"
  在 CLI 这一层**无法完全满足**的地方 —— 要让它读不到，只有"给产品加注入注册表的开关"
  （AGENTS.md 明令禁止）或"让 `path diff` 接受两份快照文件"（那是另一件事，票据没有）两条路。
  缓解是"只读 + 每条用例断言跑前跑后逐字节相同"，以及 core/platform 两侧的 106 条假后端用例。
- **`--dry-run` 与真写"产出同一个计划"是间接证明的**：契约测试比的是两次 `--json` 的
  `scopes` 逐字节相同 + `--dry-run` 后真实注册表哈希未变；"同一份计划对象"这条更强的断言
  只有在能注入注册表的后端上才做得到（那是 platform 的 35 条用例在做的）。
- **长度预算的 `exceeded` → 拒绝写入**只有夹具用例，真机离悬崖还很远：**两个分母都要说出来** ——
  原始快照的 `budget.effectiveChars` 是 **1780**（用户级 773 + 机器级 1007，余量 **6411**），
  而 §7 那次 `--only add --dry-run` 的计划里是 **1815**（用户级重建后 808 + 机器级 1007，余量 **6376**）。
  （我一度把文档里的 6376 当成过期数字改成 6411 —— 错的：6376 对应的是"加了 maven 那条之后"的计划，
  6411 对应的是"原始快照"。同一份文档里两个都出现，就必须各自说清是哪个口径。）
  没有构造过一条 8192 字符的真实 `PATH`（那需要先改开发机的 `PATH`，本票禁止）。
- **机器级改动之后的长度**没算：`budget` 是"机器级原文（**未改**）+ 重建后的用户级原文"，
  如果用户之后提权把机器级也应用了，长度会更大。验收文档如实记这一条。
- **拼写疑似只问本机侧**：目标侧（另一台机器）的父目录在这台机器上看不见，
  所以跨机快照里"目标侧那一条是不是错写"我们不判（判不了的不猜）。
- **`no_op_classes` 覆盖的是"类"，不是"条目"**：`--pick` 一条 `keep` 会让 `keep` 进
  `no_op_classes`；而一条被输出不变量跳过的 `add` 会让 `add` 进 `no_op_classes`、
  同时 `applied` 里记一条 `class = fix` 的条目 —— **两处口径刻意不同**
  （前者是"你选了什么"，后者是"实际改了什么"），CLI 的打印必须分清，别把 `applied`
  当成"做了什么"的唯一来源。
- **`move` 的定义对"两条相同目录互换位置"会报 2 条**（共同序列里两边名次都变了），
  而"下标相等"的判据会报 0 条。我们选了能表达"顺序变了"的那一种（决策 142），
  本机 `move = 0`，所以这条只在夹具里有样本。
- **`suggestion` 的编辑距离阈值是 2**：`tool` → `tools` 是 1，但它也会把
  `C:\a\bin` → `C:\a\bing` 这种报出来。宁多报（它只是建议、不纠错）。
- **`too-long` 的"至少要少 N 条条目"是下界，不是精确解**：`entries_to_drop` 按最长的先丢贪心算，
  真实的最优解可能少丢一条（消息里写了"下界"两个字）。**不许拿它当精确值断言**。
- **`--dry-run --json` 遇到 `exceeded` 的那句通知走 stderr**：`--json` 的 stdout 一个字节没变
  （JSON 键由 core 拥有，多一个键就是第二份事实），代价是"只读 stdout 的脚本"看不到那句警告 ——
  但 `budget.level == "exceeded"` 本身就在 stdout 里，信息没丢。人类输出模式下这句话在正文里。
- **"机器级永不落盘"是这张票的安全前提**（决策 136）：`run_apply` 只对用户级调
  `plan_rewrite` / `apply_rewrite`，所以即便超长守卫被写坏，真跑下去也只会走到"用户级无变化 →
  `will_write() == false` → 不写不广播"。C 那条超长用例的夹具**刻意用机器级条目**造场景，
  正是为了让"守卫被写坏"不可能等于"真机 `PATH` 被写坏"。**如果将来有人把机器级接上写路径，
  那条用例的失效模式就从"测试红"变成"真机 `PATH` 被写坏 8191+"** —— 这一条已写进 `docs/DESIGN.md` §1.18。
- **验收脚本有一个 `-SkipWrite` 开关**（跳过 §6 的真实写入，只验只读部分）。它存在的理由是
  "调脚本本身时不许动 `HKCU`"；**权威的那一次必须不带这个开关跑**（§3 的 115 条就是不带开关的）。

## 6 门禁与复现

| 门禁 | 命令 | 结果 |
| --- | --- | --- |
| 格式 | `cargo fmt --all --check` | exit 0（无输出） |
| 测试 | `cargo test --workspace` | **1107 passed / 0 failed**（#14 时是 974，本票 +133） |
| 静态检查 | `cargo clippy --workspace --all-targets -- -D warnings` | exit 0（无输出） |
| 真机验收 | `pwsh -NoProfile -File scripts/acceptance-L1-15.ps1` | **`checks_passed=115 checks_failed=0 checks_skipped=0 verdict=PASS`**，`real_write_restored=True` |
| 验收脚本自检 | 同上加 `-SelfTest -SkipBuild -SkipWrite` | **exit 1**（最后一条检查故意失败：一条不能失败的验收不是验收） |

本票新增/改动的测试分布（`cargo test --workspace` 的逐套 `test result: ok`）：

- `tuoen-core`：380（其中 `pathdiff` 71 = 主文件 55 + `typo.rs` 9 + `test_support.rs` 7）
- `tuoen-platform`：82 单测 + 29 集成（`path.rs` 的用例从 6 → 28）
- `tuoen-cli`：162 单测 + `pathdiff_contract` **20** + `path_contract` 16 + `pin_contract` 25 +
  `capture_contract` 11 + `doctor_contract` 9 + `manage_contract` 23 + `shim_contract` 25 +
  `cli_contract` 10 + `detect` 13 + `catalog` 13 + `real_machine_acceptance` 5

复现本票的验收（**注意：§6 会真的写一次 `HKCU\Environment\Path`，写完立刻还原并用哈希证明**）：

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"
pwsh -NoProfile -File scripts\acceptance-L1-15.ps1                     # 权威的那一次
pwsh -NoProfile -File scripts\acceptance-L1-15.ps1 -SelfTest -SkipBuild -SkipWrite   # 必须 exit 1
```

脚本自己会在 §6 之前把原文与类型备份到 `%TEMP%\_l115_hkcu_backup_script.txt` / `.kind`，
并在**写入之前**先做一次"还原机制自检"（把当前值原样写回一次 + 断言哈希未变）。
`finally` 里还原之后会**当场**复核哈希，不一致就红字喊出来并指出备份在哪。
