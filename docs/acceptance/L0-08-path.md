# L0-08 · `PATH` 读写（禁 setx）+ 8191 预算 + 遮蔽与用户名依赖检测 · 验收记录

**票据**：[#8](https://github.com/Muelsysel/tuoen/issues/8) · **状态**：通过
**代码**：`crates/platform/src/path.rs`（引擎）、`crates/platform/src/sys.rs`（唯一的注册表写调用）、`crates/platform/src/registry.rs`（`Registry` 的写方法）、`crates/cli/src/path*.rs`（命令）
**测试**：`crates/platform/tests/path_contract.rs`（26 条）、`crates/platform/src/path.rs` 内 7 条单元测试、`crates/cli/tests/path_contract.rs`（进程边界）
**机器可读工件**：`docs/acceptance/L0-08-path-machine.txt`（由 `scripts/acceptance-L0-08.ps1` 生成）
**真机探针**：`crates/platform/examples/path_probe.rs`（16 项自检，写的是 `TUOEN_PATH_PROBE`，**从不碰真实 `Path`**）

> 这份文档只写**怎么验的**与**验出了什么**。设计的理由见 `docs/DESIGN.md` §1.7。

---

## 0. 一句话结论

`PATH` 的读、分析、计划、落盘在真机上跑通了，而且**有一条自我证明**：探针在开始和结束
各用 `GetValue(..., DoNotExpandEnvironmentNames)` 快照一次两个作用域的 `Path`（类型 + 原始字节），
**两份逐字节相同**。写机制的往返只碰 `TUOEN_PATH_PROBE`，跑完删除，无残留。

---

## 1. 验收标准逐条

| # | 标准 | 判据（可失败） | 证据 | 结果 |
|---|---|---|---|---|
| ① | 读 `PATH` 用 `DoNotExpandEnvironmentNames`，**不展开**用户的值 | 报出来的 `raw` 与注册表里的原始字节**逐字符相等** | 低层读法是 `RegEnumValueW`（**从不展开 `REG_EXPAND_SZ`**）；探针把两个作用域都对比了一遍（`两个作用域的 raw 都等于注册表里的原始字节`） | **通过** |
| ② | 写类型规则：值含 `%` → `REG_EXPAND_SZ`，否则保留原类型，再否则 `REG_SZ` | 三条各有用例；真机上真的落得下去 | `write_type_for` 三条单元测试 + `a_plain_directory_keeps_the_original_type`；探针在真注册表写了含 `%`（读回 `ExpandSz`）与不含 `%`（读回 `Sz`）各一次 | **通过** |
| ③ | 写入后广播 `WM_SETTINGCHANGE`，参数按票据 | `SendMessageTimeoutW(HWND_BROADCAST, 0x1a, 0, L"Environment", SMTO_ABORTIFHUNG, 5000, &out)` 被真的调用 | `sys::broadcast_environment_change()`；探针实测收到 0 个应答（**0 不代表失败**，见 §4） | **通过** |
| ④ | **绝不调用 `setx`** | 源码里没有**带引号**的 `setx` 字面量（那才会被当程序名传出去） | 验收脚本预检：`"setx` 命中 **0** 处；`setx` 作为文字出现 14 次，**全部是文档里警告不许调用它** | **通过** |
| ⑤ | `PATH` 长度预算告警（8191 悬崖） | 三档边界钉死；预算按**生效**长度算 | `the_two_cliffs_are_not_the_same_number`（6143/6144/7371/7372/8191/8192 六个边界）；真机实测 2107 / 1780、档位 `ok`、余量 6084 | **通过** |
| ⑥ | 遮蔽检测：报告"这 N 个条目遮蔽了我们" | 生效顺序里第一个命中该命令的目录不是我们的 shim 目录就是遮蔽 | 4 条假机器用例（机器级遮蔽 / 生效顺序 / 我们在前面 → 一条都不报 / shim 目录不存在 → 一条都不报）；**真机实验**报出 2 条，恰好是票据点名的两个例子（见 §5） | **通过** |
| ⑦ | 用户名依赖检测 | 报出硬编码 `C:\Users\<名>\…` 的条目，并标出是否在机器级 | 真机 **12 条，其中机器级 2 条**（完整清单见工件 §2）；`hardcoded_username` 的单元测试含"含 `%` 的不算"反例 | **通过** |
| ⑧ | 所有 `PATH` 变更**先出 plan 再 apply**；`--dry-run` 走同一套代码路径 | `run(..., dry_run=true)` 与 `dry_run=false` 给出**同一个 `PathPlan`**，前者不写盘 | `dry_run_produces_the_same_plan_without_writing`（断言两个计划 `==`）；验收脚本 §5 断言 `--dry-run` 前后 `show --json` 逐字节相同 | **通过** |
| ⑨ | 测试不得触碰真实 `HKCU\Environment` | 全部走 `FakeRegistry` / `MachineFixture`；CLI 只测只读与 `--dry-run`；真机写入只由探针做，且只碰别的值名 | `crates/platform/tests/path_contract.rs` 26 条全部使用假后端；验收脚本**独立于我们的代码**快照 `Path` 并比对 | **通过** |
| ⑩ | `cargo test` 与 clippy 通过 | 见 §6 | 见 §6 | **通过** |

---

## 2. 真机的原始数字（`L0-08-path-machine.txt`）

| 量 | 值 |
|---|---|
| 机器级 `Path` | **1007** 字符 / `REG_SZ` / 33 条（下标 21 是**空条目**） |
| 用户级 `Path` | **773** 字符 / `REG_SZ` / 16 条（无空条目） |
| 注册表合计 | 1780 字符 |
| 生效 `PATH`（进程里那条） | **2107** 字符 —— 多出的 327 字符是**进程注入**的 6 条 |
| 档位 / 余量 | `ok` / 6084 |
| 重复 | 12 组 |
| 失效条目 | 7 条 |
| 用户名依赖 | 12 条（机器级 **2** 条） |
| 遮蔽（默认那一跑） | **0** —— 我们的 shim 目录还不存在，我们一条命令也没发布 |
| 遮蔽（`--shadow-check` 实验） | **2** —— `java` 与 `node`，遮蔽者都是机器级条目（见 §5） |

**票据里的数字已经不准了，这本身是一条结论。** 票据写的是「用户 725 + 机器 978 = 1704」、
「机器级是 `REG_EXPAND_SZ` 却零变量」、「用户名依赖 11 条」。现在的实测是 773 + 1007 = 1780、
机器级是 `REG_SZ`、用户名依赖 12 条。用户级从 725 涨到 773 是**本会话自己干的**
（把 MinGW 的 `bin` 加进了用户级 `PATH`，+48 字符）；机器级那 29 字符与类型变化**不是我们做的**
（本会话从未写过机器级），说明这台机器的 `PATH` 在被别的软件改。

> 教训：**不要把这些数字写进断言**。`PathBudget` 的档位只能按"当前实测"算，
> 探针与脚本断言的是**结构与自洽性**（`余量 + 生效长度 == 悬崖`、档位是四个 slug 之一），
> 不是具体数值。唯一被钉死的数值是那两个**平台常量** 1024 与 8191。

---

## 3. 写机制：怎么证明"真的能写"，以及"绝不碰 `Path`"

假注册表能证明**逻辑**，证明不了**Win32 调用真的写得进去**。所以 `path_probe.rs` 在真机上做了往返：

| 步骤 | 判据 | 实测 |
|---|---|---|
| 写 `TUOEN_PATH_PROBE = %USERPROFILE%\tuoen-probe`（含 `%`） | 读回来必须是 `ExpandSz` 且值原样 | 通过 |
| 写 `TUOEN_PATH_PROBE = C:\tuoen-probe`（不含 `%`） | 读回来必须是 `Sz` | 通过 |
| 同一个值名再写一次 | 枚举出来**只有一条** | 通过（同名条目数 = 1） |
| 删除两次 | 两次都成功（幂等），之后读不到 | 通过 |

**自我证明（这一票最重要的一条）**：探针开头与结尾各快照一次
`HKCU\Environment\Path` 与 `HKLM\...\Environment\Path` 的**类型 + 原文**，
`before == after` 必须为真，并写进 `SUMMARY` 的 `path_untouched=1`。
验收脚本**另外**用 PowerShell 的 `GetValue(..., DoNotExpandEnvironmentNames)` 独立做了一遍同样的比对
（不经过我们的代码）—— 两套独立实现都得出"一个字节都没变"。

**`setx` 一次都没跑过。** 因为跑它就是在改这台机器。能靠 grep 钉住的东西，
不需要真跑一遍去证明 —— 这是这一条验收的取舍，写在这里以免被当成遗漏。

---

## 4. 广播：它做了什么，以及它**做不到**什么（都是实测的）

- 调用形式：`SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE /*0x1a*/, 0, L"Environment", SMTO_ABORTIFHUNG /*2*/, 5000, &out)`。
- 实测**收到 0 个顶层窗口的应答**。`0` **不代表失败** —— 没有顶层窗口愿意应答是正常的
  （`SendMessageTimeoutW` 会在超时后返回，我们不检查它是否"成功"）。
- **它不是"所有进程都更新了"的证据。** 环境块是 `CreateProcess` 时从父进程复制的，
  所以：
  - 广播之后，**我们自己**进程的环境里读不到 `TUOEN_PATH_PROBE`（实测 `false`）；
  - **新起的**子进程（父进程是我们）看到的是 `[TUOEN_PATH_PROBE=%TUOEN_PATH_PROBE%]`
    —— 子进程拿到的是**我们**的环境块，不是注册表里最新的那一条。

这两条是 CLI 必须打印"**新开一个终端才生效**"的直接依据（实测，不是推测）。
已经在跑的 cmd / PowerShell / IDE / 服务，**永远不会**拿到新环境。

### 4.1 生效顺序到底是什么（真机实测的公式）

这一条是本票后半段**推翻了自己的实现**才搞清楚的东西，记在这里因为它同时是 ADR-0002 的依据。

```
进程 PATH  =  [启动器注入] + 机器级 Path + 用户级 Path
```

实测（在本机的一个普通 PowerShell 里）：

- 逐字节比 `$env:Path` 与 `"<机器级>;<用户级>"`：**长度相等（1781）但内容不同** ——
  进程那条的**第 0 条**是 `C:\Program Files\WindowsApps\Microsoft.PowerShell_7.6.6.0_x64__8wekyb3d8bbwe`
  （PowerShell 作为 MSIX 包被激活时注入的别名目录），**它排在机器级前面**。
- 去掉那一条之后，前 31 条与"机器级 + 用户级"逐条一致 → **机器级条目确实全部排在
  用户级条目之前**，ADR-0002"用户级工具永远输掉名字冲突、只能靠 shim 抢名字"成立。
- 规范化去重后：期望 30 个不同目录、进程 29 个 —— 差的正是下面这条。

**"陈旧的终端"是平台事实，不是数据错误。** 本机的那个 shell 里缺两条注册表条目
（`C:\Program Files\GitHub CLI\` 与 `C:\Users\Muelsyse\.local\toolchains\mingw64\bin`），
原因是**它开得比我加那两条更早** —— 环境块只在 `CreateProcess` 时复制。
所以 `analyze` 的实现是：**以进程里那一条为准，再把注册表里有、进程里没有的追加在末尾**。
把它们丢掉会让遮蔽判定在一个陈旧的终端里**漏报**。（这条修正直接删掉了第一版
"生效顺序 = 机器级 + 用户级"的写法 —— 那个写法在**本机**就是错的：`cargo` / `rustup` / 宿主
各自往前面插自己的目录，一次 `cargo run` 里数出 6 条注入。）

---

## 5. 遮蔽检测：真机上是**真的报出来了**

遮蔽的判据是"我们的 shim 目录里发布了哪些 `<名字>.exe`"。本机这个目录**一开始并不存在**
（我们一个 shim 都还没装），所以默认那一跑报 `shadowed=0` —— 那证明的是
"目录不存在时不报假遮蔽"，**证明不了"遮蔽报得出来"**。

于是探针带一个 `--shadow-check`：往我们的 shim 目录里放**两个 0 字节的空文件**
（`java.exe` 与 `node.exe`），再分析一次。**只看文件，`PATH` 一个字节都不动。**

真机结果（`docs/acceptance/L0-08-path-machine.txt` §3.5）：

```
遮蔽：`java` 被 机器 级条目 `C:\Program Files (x86)\Common Files\Oracle\Java\java8path`（java.exe）抢在前面
遮蔽：`node` 被 机器 级条目 `C:\nvm4w\nodejs`（node.exe）抢在前面
```

**这正是票据 ⑥ 点名的两个例子**（"本机默认就会发生：`C:\nvm4w\nodejs` 与 Oracle `java8path`"）。
报告本身还被**独立核对**了三件事，而不是只看它非空：

1. 每条报告的 `by.value\<file>` 真的存在（直接问文件系统，不信任分析结果）；
2. 被指为遮蔽者的目录是生效顺序里**第一个**持有该命令的（前面每一条都不持有）；
3. 我们放进去的每一条命令**都**出现在报告里，且遮蔽者是先用另一段代码算出来的那个目录。

这三条的前两条第一版都写错过，值得记：

- 第一版的顺序判据是"遮蔽者的下标 < **我们的 shim 目录**的下标" —— 但我们的 shim 目录
  **根本不在 `PATH` 上**（这正是 `tuoen path add <shims>` 存在的理由），于是下标为 `None`，
  判据直接失败。真实的不变式不是"我们排在后面"，而是"**没有比它更早的目录持有这个文件**"。
- 第一版只放了一个文件，而且是从 `PATH` 里"第一个找得到的 `.exe`"挑的 —— 挑中了 Oracle 的
  `java8path`，于是只证明了 `java` 那一条。改成先放票据点名的两个名字之后，两个例子都覆盖到了。

探针跑完把两个空文件删掉；`shims` 目录是它建的就删掉，父目录 `%LOCALAPPDATA%\tuoen`
**只在空的时候**才删（`remove_dir` 的语义正好是"空才成功"）—— 本机那里有真的 store，所以它被正确地留下了。

### 5.1 活的端到端：**真的 shim**，真的被抢（`acceptance-L0-08.ps1` §5.5）

`--shadow-check` 那一跑是**人造前提**（0 字节空文件）—— 它能证明"判据对"，但证明不了
"端到端真的会发生"。所以验收脚本里补了一段**真的**：它会

1. 用真实 store 里已安装的 `node 24.19.0` 跑 `tuoen shim add node`（生成 4 条真 shim）；
2. 跑 `tuoen path add <shims> --dry-run`，断言人类输出里有遮蔽那一段；
3. 断言 `path show --json` 的 `shimCommands`（**分母**）等于盘上的文件数；
4. 跑 `tuoen shim remove node`（删掉其中一条），断言它**提示了剩下的兄弟命令**；
5. 把这一段生成的那几条全删掉，**核实目录回到了跑之前的样子**。

真机输出（`L0-08-path-machine.txt` §5.5）：

```
遮蔽：我们发布的命令里有 **4 条**被别的目录抢在前面（一共 4 条）。

  ! `corepack` → 先命中的是 `C:\nvm4w\nodejs\corepack.cmd`（机器级）
  ! `node`     → 先命中的是 `C:\nvm4w\nodejs\node.exe`（机器级）
  ! `npm`      → 先命中的是 `C:\nvm4w\nodejs\npm.cmd`（机器级）
  ! `npx`      → 先命中的是 `C:\nvm4w\nodejs\npx.cmd`（机器级）
```

顺手把这一条也验掉了：**我们的 shim 真的能跑**（在真机上直接执行那 4 个 `.exe`）：

```
node.exe --version     → v24.19.0    （与 C:\nvm4w\nodejs\node.exe 一致）
npm.exe --version      → 11.17.0     （与 C:\nvm4w\nodejs\npm.cmd 一致）
npx.exe --version      → 11.17.0
corepack.exe --version → 0.35.0
```

这是 L0 全链路第一次**在真机上端到端**跑通：下载 → 归档 → store → junction（`node/current`）
→ shim（生成时写死目标）→ 执行。它同时是 L0-07 那条"shim 真能转发"的 CLI 侧复验。

### 5.2 两个由这一段抓出来的真问题

**① `shim add` 与 `shim remove` 的粒度不对称（已修）。** `tuoen shim add node` 一次生成
4 条，而 `tuoen shim remove node` **只删 `node.exe`** —— 因为 `remove` 收的是**命令名**。
不对称本身合理（`remove npm` 必须能只删 npm），但**静默**会让用户以为删干净了：实测
`remove node` 之后盘上还剩 3 条。修法是**报告**（决策 77）：删完列出同工具仍留在盘上的
命令，并给出**可以直接抄**的完整命令 `tuoen shim remove corepack npm npx`。
判据里最容易漏的一条是"**盘上真的还在**才算"—— 只按表推断的话，删一个从没生成过全集的
工具会报出一堆不存在的东西。

**② 一条会被误报的测试断言（已修）。** `the_real_tuoen_home_was_not_touched_by_this_files_tests`
（在 `path_contract.rs` 与 `shim_contract.rs` 里各有一条）原来断言"真实的 shim 目录
**不存在**"。那是**机器状态**，不是被测代码的性质：任何真的用 `tuoen shim add` 装过东西的人
都有这个目录，于是那条断言会在最不该红的时候红 —— 本票的真机验收就把它踩红了。
改成：`real_home_listing()` **往 `shims` 里看一层**（只列顶层的话，"在真实 shim 目录里
写了一个 shim"这种最严重的事故看不见，因为 `shims` 这个名字本来就在那儿），然后只断言
**跑完前后逐项相同**。这是决策 50（"断言必须能失败"，代价是**也能被误报**）的第二次教训，
记成决策 78。

---

## 6. 重复检测：规范化比较比"原文比较"多找到 2 组

这一条是**独立交叉核对**时发现的，值得单独记：用 PowerShell 按"原文（去空白）小写"分组，
找到 10 组重复；我们的实现找到 **12** 组。差的 2 组是**只差一个结尾反斜杠**：

```
机器#14 = [C:\Program Files\dotnet]          机器#30 = [C:\Program Files\dotnet\]
机器#27 = [C:\Program Files\MySQL\...\bin]   用户#9  = [C:\Program Files\MySQL\...\bin\]
```

`normalize_entry` 去掉多余结尾反斜杠（但保留 `C:\` 这种根），所以它们被正确地合成一组；
顺带 `C:\Software\tool\`（用户#11）也被并进了 `c:\software\tool` 那组，于是它从 ×2 变成 ×3。
两条独立实现的分歧**全部由这个已知差异解释**，没有第三条无法解释的分歧。

---

## 7. CLI 命令面：`tuoen path show | add | remove`

| 命令 | 行为 | 真机实测 |
|---|---|---|
| `tuoen path`（不带子命令） | ≡ `show` —— **"看"是最安全的默认动作**，而"敲了 `tuoen path` 就改了 `PATH`"是最不该发生的默认动作 | `path --help` 里写明了这条 |
| `tuoen path show` | 只读；报预算档位与余量、两个作用域的条数与类型、重复、失效、用户名依赖（标出机器级）、遮蔽、进程注入项 | `--json` 22132 字节、`command: "path.show"`、退出 0；`data.budget.effectiveChars = 1781` —— 与我在 PowerShell 里独立量到的 `$env:Path.Length` **完全一致** |
| `tuoen path add <dir>` | 追加到**用户级**末尾（决策 74：只追加、不 prepend） | `--dry-run` 打出 `长度：1780 → 1806 字符（**注册表口径的下界**，不含进程注入；档位 ok；还剩 6385 字符）` 与 `+ 追加 …（第 16 段之后）`，退出 0 |
| `tuoen path remove <dir>` | 删掉**全部**出现；删自己的 shim 目录 → 拒绝（`protected-shim-dir`，非零退出） | 见 §1 ⑧ 与验收脚本 |
| 参数含 `;` | 当场拒绝、非零退出（决策 66） | 退出 **1**，消息说明"引号保护不了它" |

**`--dry-run` 不写盘这件事是被独立验证的**，不是声称：验收脚本在 `--dry-run` 前后各跑一次
`show --json`，断言两次输出**逐字节相同**（这是决策 50 的推论 —— 逐字节稳定性必须在**同一状态**
下比较）。CLI 的进程边界测试同样只碰只读路径与 `--dry-run`。

**`add` / `remove` 之后还会印一段遮蔽报告**（决策 73、76）：`path add` 只把我们的目录
**追加到末尾**，所以它**修不好遮蔽** —— 名字冲突永远是先命中的赢。报告逐条点名
"哪条命令被哪个目录抢了"，并按作用域给出可执行的下一步（用户级 → `tuoen path remove <那条>`；
机器级 → 要管理员权限，**tuoen 不做顺手提权**）。它只在两种情况下印：真被抢了，
或者这次加的就是我们自己的 shim 目录（那是"让 tuoen 的命令能用"这个意图最该听的结论）。

这一段有一个**必须是假话也说不出来**的细节：分母。空 shim 目录上的
`shadowedShimCount=0` 有两种完全不同的成因 —— "我们的命令都赢了"和"我们一条命令都还没发布"。
所以 `PathAnalysis` 带上 `shim_commands`，空目录时说的是"**没得比**"而不是"都没输"。
本机一开始就踩在这个坑上（shim 目录还不存在，报告却已经在说"都是第一个被命中的"）。

`path.plan` 的 `--json` 因此多了 4 个键：`shimDir`（`null` = 这次没查遮蔽，
与 `[]` = 查了一条都没被抢**必须分得开**）、`shadowedShims`、`shadowedShimCount`、
`shimCommands`（分母）；`path show --json` 多了 `shimCommands`。

`--json` 的稳定性判据：同一状态下两次调用**逐字节相同**，键是 camelCase，取值是稳定 slug
（`ok` / `warning` / `critical` / `exceeded`、`already-present` / `absent` / `protected-shim-dir`），
中文只出现在 `message` 与人类输出里。

---

## 8. 诚实未覆盖（这一节必须存在）

1. **真实写入 `Path` 的端到端路径没有在本机跑过。** 探针只写别的值名。
   `apply` 写 `Path` 走的是**同一个** `Registry::set_value`，但"写整条 773 字符的 `Path`"
   这件事本身没在真机上发生过 —— 那需要一个愿意被改 `PATH` 的机器。
2. **机器级 `Path` 的写入完全没做，也不打算做**（要提权，而本项目不做"顺手提权"）。
   `plan_add` / `plan_remove` 只读机器级（用来算预算与遮蔽），从不写它。
3. **8191 悬崖没有被真实触发过。** 本机 2107 字符，离悬崖很远。三档边界是单元测试钉的，
   不是真机跑出来的。
4. **`setx` 的 1024 裁剪与"永久展开"没有在本机复现**（复现就是在改这台机器）。
   票据里的机制描述与研究文档的结论一致，但本机没有测量数据。
5. **遮蔽检测的"人造前提"那一跑仍然是 0 字节空文件**；§5.1 补的是另一条**真的**
   路径（真 shim、真 store、真 junction、真被抢），但它是**脚本里的一段**，
   不是 `cargo test` 里的一条 —— 因为测试不许往真实家目录写 shim。
   代价：这一段只在"真实 store 里装过工具"的机器上跑；没有就报 skip（**不是**通过）。
6. **`Path` 的并发写没有测。** 两个 `tuoen` 同时改 `PATH` 会互相覆盖
   （我们都是"读整条 → 写整条"）。V1 不做锁；在文档里说清楚，不假装它安全。
7. **`%VAR%` 条目的"失效"判定是故意不做的**（含 `%` 就不判失效，避免假阳性）。
   代价是：`%NOPE%\bin` 这种真的坏掉的条目我们**不会**报出来。
8. **用户名依赖的判据只有 `\Users\`。** 换机后同样会坏的还有 `C:\Documents and Settings\…`
   与自定义 profile 路径，我们没有覆盖。
9. **`path add|remove` 之后的那段遮蔽报告只在 `node` 上端到端验过**（因为
   `core::shim::shim_commands` 目前只实测了 node 一家）。别的工具还没有 shim 表，
   所以"报告"对它们是空转 —— 这与"表是空的"是同一件事，不是这段逻辑的缺陷。
10. **兄弟提示只覆盖 `core` 表里认识的工具。** 将来别的工具也往 shim 目录放文件时，
    反查不到工具的名字不会触发提示（那是刻意的：宁可不说，也不猜）。

---

## 9. 门禁与复现

```powershell
# 全量门禁
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check

# 真机探针（只写 TUOEN_PATH_PROBE，跑完删除）
cargo run -p tuoen-platform --example path_probe

# 真机验收（预检 + 探针 + 独立快照比对 + CLI dry-run + 真 shim 遮蔽端到端）
pwsh -File scripts/acceptance-L0-08.ps1
# 自检：证明这条脚本真的会失败（必须 exit 1）
pwsh -File scripts/acceptance-L0-08.ps1 -SelfTest

# 真机只读用例（默认跳过）
$env:TUOEN_REAL_MACHINE_TESTS = '1'
cargo test -p tuoen-platform --test path_contract the_real_machine_reads_without_writing

# 清场核对：探针不留残留
Get-Item HKCU:\Environment | Select-Object -ExpandProperty Property
# 清场核对：真 shim 那一段跑完，shim 目录回到原样（本机是 0 个文件）
Get-ChildItem "$env:LOCALAPPDATA\tuoen\shims" -File | Measure-Object
```

`scripts/acceptance-L0-08.ps1` 的每条期望都走 `Check`，**退出码跟随失败数**（设计决策 50）。
`-SelfTest` 会故意加一条必然失败的期望：它必须 `exit 1`，否则说明脚本只会骗人。
第 5.5 节在自己动过的目录上**先快照、只删自己生成的、跑完核实还原**（探针自证的同一条原则，决策 71）。
本机最新一跑：**58 项通过 / 0 项失败，exit 0**（含 5.5 的 12 项）。
