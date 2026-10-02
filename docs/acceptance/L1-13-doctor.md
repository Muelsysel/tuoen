# L1-13 验收：环境体检（`tuoen doctor`）

`doctor` 是**只报告、不修改**的命令：它的产出只有两类 —— `--json` 的成功载荷与中文人类输出。
所以这一票的验收重点不是"它有没有写坏东西"，而是**它说出去的话是不是真的**。

这也是这一票在票据里的定位：**产品可信度的第一道门**。把"`PATH` 上确认可执行"与"注册表声称
已装但文件缺失"并列展示，会让用户看一眼就不再信任这个工具；反过来，一条**看起来完全合理
的错话**比一次崩溃更难被发现。

- 验收脚本：`scripts/acceptance-L1-13.ps1`（**90 条检查**，`-SelfTest` 必须 exit 1）
- 机器工件：`docs/acceptance/L1-13-doctor-run.txt`
- 设计决策：`docs/DESIGN.md` §1.14（95–107，实现期）+ §1.15（108–112，验收期）

## 1 逐条验收

### 1.1 票据「验收标准」四条

| 验收标准 | 证据 |
| --- | --- |
| `cargo test --workspace` 全绿 | **823 passed / 0 failed**（`doctor` 相关 93 条：检查项 74 + `doctor.rs` 3 + CLI 单元 7 + 进程边界契约 9） |
| clippy `-D warnings` 干净 | `cargo clippy --workspace --all-targets -- -D warnings` → exit 0，零 warning |
| 真机跑一次 `tuoen doctor` 并把完整输出贴进评论 | 见 §2 与 issue 评论（`--json` 8,365 字节 + 人类输出 300+ 行） |
| **任何与取证报告不符的地方都要在评论里说明** | §2 的对照表逐条列出 7 处不符，每一处都给了原因 |
| 断言无写操作 | 脚本 §7：两个作用域的 `Path` 原文 + 类型 + SHA256、`%LOCALAPPDATA%\tuoen`（含 `shims` 一层）、store 清单、Lxss 子键、持久变量个数，**跑前跑后逐项相同** |

### 1.2 票据「必须有的用例」

- **固定装置，不用本机实时状态**：`doctor` 的检查项全部是 `fn(&MachineFacts) -> Vec<Finding>`
  的**纯函数**，用例直接拼 `MachineFacts`（`crates/core/src/doctor/checks/*.rs` 的 `tests` 模块）。
  本机实时状态只出现在 §1.3 那个"唯一碰机器的只读层"里。
- **每个检查项至少一个正例 + 一个反例**：74 条用例按检查项成对出现，反例的名字直接说明它想
  守住什么 —— 例如 `distinct_values_produce_no_duplicate_finding`、
  `a_portable_spelling_is_not_a_hardcoded_username`、`plain_directories_produce_no_reparse_finding`、
  `only_absolute_entries_produce_no_relative_finding`、`no_shims_means_nothing_to_shadow_and_nothing_is_reported`、
  `a_healthy_machine_produces_nothing_at_all`。
- **`path.length-budget` 的四个边界值**：`the_budget_boundaries_are_exactly_what_the_ticket_asked_for`
  逐字断言 `1719 → ok（不报）`、`8190 → critical（warn）`、`8191 → critical（warn）`、
  `8192 → exceeded（error）`。**8191 与 8192 的差别**是"`cmd.exe` 还在看 `PATH`"与
  "`cmd.exe` 已经完全忽略它"，两条消息的措辞不同，而用例断言的是 `severity` 与 `level`。

### 1.3 票据「硬性约束」

> **诊断检查项的测试必须用固定装置（fixture），不用本机实时状态** —— 否则测试结果会随开发机漂移。

实现上把它变成了**架构**，而不是纪律：事实与判断分成两层。

- `crates/core/src/doctor/facts.rs` 的 `collect_facts(ctx, opts) -> MachineFacts` 是**唯一**
  碰机器的层（只读：注册表、`PATH` 原文、磁盘、注入的进程运行器）；
- `crates/core/src/doctor.rs` 的 `diagnose(&MachineFacts) -> Vec<Finding>` 是**纯函数**，
  四族检查项全部是它的子函数。

于是"测试里不许出现真机状态"不需要靠人记得：检查项**拿不到**机器。

### 1.4 票据「明确不做」

`doctor --help` 里只有 `--json` 与 `--no-probe`。`--fix` / `--strict` / `--fail-on`
一律被 clap 以**退出码 2** 拒掉（脚本 §0 三条用例钉住）。帮助文本里连 `--fix` 这五个字符
都不出现（"顺手修一下"的开关用中文表述），理由写在源文件文档里：解释"为什么没有 `--fix`"
必然要提到它，而契约用例断言的是**帮助里没有它** —— 两者不能同时满足时，选严格断言 +
文档承载理由（同 `AGENTS.md` 里 `setx` 那条 grep 约定）。

**`doctor` 永远退出 0**：这次体检跑完了就是跑完了，"发现了 7 条 error"不是命令失败。
脚本要判据请读 `--json` 的 `counts.error`。

## 2 真机数字：与票面预期逐条对照

跑法：子进程 `PATH` 固定成**新终端会看到的那一条**（机器级原文 + `;` + 用户级原文，展开一次），
因为 `pathEntries` / `effectiveChars` 是**从进程 `PATH` 量的**。用带 cargo/rustup 前缀的
`PATH` 跑，分母就不是 49 而是 55 —— **分母必须说出来**。

```
SUMMARY counts error=7 warn=13 info=9 findings=29
        path_entries=49 env_vars=32 tool_rows=27 resolved_commands=23 wsl_distributions=2 shims_on_disk=0
        raw_user_chars=773 raw_machine_chars=1007 effective_chars=1781 level=ok
```

| 检查项 | 票面预期（取证期） | 真机实测 | 说明 |
| --- | --- | --- | --- |
| `path.duplicate` | 10 组（忽略大小写 12 组） | **12 组 / 富余 18 条** | 票面括号里自己写了"忽略大小写 12 组"，实测取的就是忽略大小写口径 |
| `path.missing` | 7 条 / 5 个不同目标 | **7 条 / 5 个不同目标** | 一致 |
| `path.username-hardcoded` | 11 条，2 条机器级 | **12 条，2 条机器级** | 机器级那 2 条与票面一致；总数差 1 是取证期与现在的 `PATH` 不同 |
| `path.shadowed` | `C:\nvm4w\nodejs`、Oracle `java8path` | **0 条** | 票面那两条是"我们的 shim **若存在**会被谁遮蔽"；本机 shim 目录是空的（`shimsOnDisk=0`），**没有 shim 就无所谓遮蔽**。判据与取证口径不同，不是 bug |
| `path.length-budget` | 1781，余量充足 | **1781（ok，不报）** | 一致。`ok` 不产生 finding —— 一个"一切正常"的发现是噪声 |
| `path.empty-entry` | HKLM `Path` 里有 `;;` | **1 条（machine#21）** | 一致 |
| `path.reparse` | 2 个（junction + symlink） | **3 条** | 票面按**目标**算 2 个；按**条目**算是 3 条 —— nvm4w 的 symlink 在机器级与用户级各有一条 |
| `path.relative` | 0 | **0** | 一致 |
| `path.non-ascii` | 0 | **0** | 一致 |
| `path.spaces` | 14/48 = 29% | **14/48** | 一致 |
| `env.missing-target` | `HALCONROOT` | **1 条：`user HALCONROOT`** | 一致 |
| `env.duplicated-scope` | `NVM_HOME` / `NVM_SYMLINK` | **2 个变量 / 4 行** | 一致 |
| `env.name-with-spaces` | `IntelliJ IDEA` | **1 条** | 一致 |
| `env.path-literal` | **0** | **5 条** | **方向②**：`REG_EXPAND_SZ` 但值里没有 `%`（两个作用域的 `NVM_HOME`/`NVM_SYMLINK` + 用户级 `OneDrive`）。见 §3.2 |
| `tool.multi-manager` | Node（nvm4w + 我们） | **1 条：`node manager=nvm4w` / `node manager=tuoen`** | 一致 |
| `tool.multiple-active` | Java 分裂 | **2 条：java + python** | 比票面**更宽**：Python 也分裂（`python`/`python3` → `WindowsApps` 的 0 字节别名，`pip`/`pip3` → `Python312\Scripts`） |
| `tool.ghost` | `Python311` | **4 条 finding / 5 个卸载键**（dotnet ×2、java 1、python ×2、wsl 1） | `Python311` 那条在里面；另外 4 个键是取证期没点到的（`.NET Host`、`Java Auto Updater`、`Python Launcher`、`WSL`） |
| `tool.global-prefix-inside-version-dir` | `npm config get prefix` = `C:\nvm4w\nodejs`（符号链接内部） | **1 条**：`origin=probe`、`reparse=symlink-dir`、`link-target=…\nvm\v24.19.0`、`version=24.19.0` | 一致，而且**探测真的跑通了**（`npm.cmd config get prefix` 经注入的运行器执行） |
| `tool.unmanaged-directory` | `C:\Dev\Tool\apache-maven-3.9.5` | **2 条**：maven + `C:\Dev\base\JDK` | 比票面多一条：`C:\Dev\base\JDK` 只被文件系统扫描提到过（`mentioned-by=filesystem-scan,path-resolution`），没有任何机制在管它 |
| `system.*` | 4 条 info | **4 条 info** | 一致：`elevated=false`、`developer-mode=absent`、`long-paths=true`、WSL 非标准位置 |

**7 处不符，两种性质**：三处是**口径不同**（`path.duplicate` 的组数口径、`path.reparse`
按目标还是按条目、`path.shadowed` 的"若存在"与"现在有"）；四处是**实测比票面宽或窄**
（`path.username-hardcoded` 11→12、`env.path-literal` 0→5、`tool.multiple-active` 1→2、
`tool.ghost` 1→4、`tool.unmanaged-directory` 1→2）。**没有一处是"代码错了而票面对了"**，
也没有一处是"票面错了而代码对了"—— 都是取证期与现在的机器状态、或判据口径的差别。

## 3 真机验收抓出来的问题（四条，都已修）

### 3.1 「两次 `--json` 逐字节相同」原来只在**同一秒**里成立（决策 108）

`crates/cli/tests/capture_contract.rs` 的 `json_is_stable_and_agrees_with_the_files_on_disk`
是 #12 的交付，它断言"两次 `--json` 除时间戳外逐字节相同"。**它在 `cargo test --workspace`
里红了**：

```
assertion `left == right` failed: 两次 `--json` 除时间戳外必须逐字节相同
  left: …"capturedAt":"2026-10-02T19:18:03Z"…
 right: …"capturedAt":"2026-10-02T19:18:04Z"…
```

根因不在产品，在那条用例自己的助手：`strip_timestamp` 按**行**删时间戳行 ——
TOML 里时间戳自己占一行，而 `--json` 的成功载荷是**一整行**，删行等于删掉整份载荷，
于是"两次相同"退化成"两份**空**串相同"（一个把两边都删光的断言永远通过）。
它平时能过，只是因为两次调用**恰好落在同一秒**里。

修法：JSON 走"只替换值、保留键与引号"的分支（`"capturedAt":"<stripped>"` 的形状本身也是
契约，键名改了这条断言仍然要红），并补一条**在旧实现下必须红**的用例
`strip_timestamp_scrubs_the_json_value_without_dropping_the_payload`。

**教训**：`断言无写操作`、`断言幂等` 这类"没变化"的断言，最容易变成"什么都没比"。
一条断言要能失败，才有资格说自己通过了。

### 3.2 三处判据太宽，会变成噪声（决策 102/103/104）

第一次全量真机诊断是 `error=15 warn=13 info=9`。逐条看下去，有三处**判据比它要抓的东西宽**：

| 判据 | 原来的口径 | 真机命中 | 收窄后 | 收窄后的口径 |
| --- | --- | --- | --- | --- |
| `tool.multi-manager` | "来源种类 ≥ 2" | 6 条 | **1 条** | 只数**真正的管理器**（`manager` / `tuoen`）；`path-resolution`、`registry-arp`、`filesystem-scan` 是"提到了它"，不是"在管它" |
| `env.duplicated-scope` | 两个作用域同名 | 5 条 | **2 条** | 排除 Windows 默认变量（`Path`/`TEMP`/`TMP`/`PATHEXT`/… 两个作用域都有是**设计如此**） |
| `tool.unmanaged-directory` 的 `only-scan` | 无条件写 `true` | 1 条自相矛盾的证据 | — | 必须**真的算出来**（`mentioned_by == ["filesystem-scan"]`），并新增 `managed_by` 字段 |

三处都补了反例用例：`three_mechanisms_without_a_manager_are_not_multi_manager`（本机 java 的形状：
`path-resolution` + `registry-arp` + `filesystem-scan` → **不报**）、
`a_windows_default_in_both_scopes_is_not_corruption_but_nvm_still_is`、
`the_windows_default_list_is_case_insensitive`。

**收窄后：`error=6 warn=13 info=9`（cargo 前缀的 `PATH`）/ `error=7 warn=13 info=9`（干净 `PATH`）。**

### 3.3 引擎给人看的占位符不许进 `evidence`（决策 111）

`tool.ghost` 第一版把检测引擎给**人**看的中文占位符原样抄进了 `evidence`：
`path=<无 InstallLocation，卸载键 {GUID}>`。于是 `doctor --json` 的**成功载荷里出现了 CJK** ——
而 `--json` 不本地化是决策 35。修法：占位符翻译成两条 ASCII 行
（`path=<placeholder>` 与独立的 `uninstall-key={GUID}`；真中文路径 → `path=<non-ascii>`）。

这条是**子代理 B 的契约用例抓到的**（它逐字断言"成功载荷无 CJK"），而我作为复核者又把它
变成了验收脚本里的一条判据：**每条 `evidence` 都必须是纯 ASCII**，而 `message` 里可以有中文 ——
"结论是数据，人话只在 `message` 里"。

### 3.4 我自己的两个假检查（脚本的 PowerShell 陷阱）

1. **`(Findings-Of 'x').Count` 在 `StrictMode` 下报"找不到属性 Count"**：PowerShell 的函数
   会把**空数组枚举成"什么都没有"**，调用方拿到 `$null`。判据本身是对的（0 条），报出来的却是
   崩溃 —— 修法是调用方一律写 `@(Findings-Of 'x').Count`。
2. **`$array -like 'pattern'` 是逐元素匹配**：我拿它当"过滤一组证据行"用，于是**整组都算命中**，
   `$_.Substring(13)` 又走了 PowerShell 的成员枚举，造出 `es`、`path=true` 这种"证据"。
   换成 `Where-Object { $_.StartsWith('…') }`。
3. 顺带：`Where-Object` 与 `Compare-Object` 的教训（决策 94）在**这一条脚本里又出现了一次** ——
   比较两个集合时两边都要**先排序**，否则比的是位置。

## 4 这台机器上真实读到的形状（值得留下的）

- **`system.long-paths=true`**（`HKLM\SYSTEM\CurrentControlSet\Control\FileSystem\LongPathsEnabled=1`）——
  与早前笔记里"键不存在"相反。**探针是权威**：三条 `system.*` 都用 PowerShell 独立核对过
  （`IsInRole(Administrator)` = False、`AllowDevelopmentWithoutDevLicense` = 不存在、`LongPathsEnabled` = 1）。
- **Java 分裂逐字确认**：`java` → `C:\Program Files (x86)\Common Files\Oracle\Java\java8path`，
  而 `javac`/`jar`/`jshell` → `C:\Dev\base\JDK\JDK8\bin`。`java8path` 是一个 **junction**
  （目标 `…\java8path_target_1783390`，2026/5/3 由 Oracle 安装器创建），而 `javapath` 是**失效条目**。
- **Python 分裂**：`python`/`python3` → `WindowsApps` 的 **0 字节 reparse**（App Execution Alias），
  `pip`/`pip3` → `Python312\Scripts`。别名执行**未验证**（我们绝不执行它）。
- **`global_prefix = C:\nvm4w\nodejs`**，`origin=probe`：`npm.cmd config get prefix` 真的跑通了，
  reparse 指向 `…\nvm\v24.19.0`，版本成分 `24.19.0` 与目标名一致。
- **WSL**：`Arch-Linux-current`（`C:\linux\Arch-Linux-current`，vhdx 1,548,746,752 字节，**非标准位置**）
  与 `docker-desktop`（`\\?\C:\Users\…\AppData\Local\Docker\wsl\main`，vhdx 100,663,296 字节，
  标准位置 → **不报**）。`\\?\` 前缀必须先剥掉再比 —— 否则标准位置的发行版会被误报。
- **`--no-probe` 的差别恰好是一条**：`tool.global-prefix-inside-version-dir`
  （`error=7` → `error=6`），`summary` 六个分量完全不变。

## 5 诚实未覆盖

- **只有一台机器**。所有真机数字都来自作者本机；`path.shadowed` 的判据在真机上**从未被触发**
  （shim 目录是空的），它只有固定装置里的证据。
- **`tool.ghost` 的判据只验证了"键在 ARP 里且没有可用 InstallLocation"**，没有验证引擎
  "哪个工具属于哪个键"的匹配规则（那是检测引擎的既有契约，本票不重测）。
- **`--only` 之类的组合**在 `doctor` 上不存在（它没有 `--only`）；`--no-probe` 只测了
  "与默认跑法差一条"，没有测"探测超时"这条路径（本机探测 300ms 内就回来了）。
- **探测超时**（`probe_timeout`）在真机上没有触发过，只有单元测试。
- **`path.length-budget` 的 8191 悬崖**只在固定装置里断言过；本机是 1781，离崖边很远。
- **`confidence` 字段**只在 `tool.multiple-active` / `tool.ghost` / `tool.unmanaged-directory`
  上出现（来自检测引擎），本机取值分别是 `executable`/`alias-ghost`、`registered-missing`、
  `directory-only`；其他检查项一律**没有** `confidence` 键（不是 `null`）。
- **人类输出的中文措辞不是契约**：契约是"中文 + 逐条点名 + 末尾两行"，措辞可以改。
- **`doctor` 的耗时**（本机 ~500ms，含探测）没有做成判据：它依赖机器负载，做判据只会变成假红。

## 6 门禁与复现

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

cargo fmt --all --check                                        # exit 0
cargo test --workspace                                         # 823 passed / 0 failed
cargo clippy --workspace --all-targets -- -D warnings          # exit 0

# 真机验收（90 条检查，只读）
pwsh -File scripts/acceptance-L1-13.ps1                        # exit 0，PASS
pwsh -File scripts/acceptance-L1-13.ps1 -SelfTest              # 必须 exit 1
```

脚本 §3–§6 的每一个数字都是**它自己**用 PowerShell 从注册表、`PATH` 原文与磁盘数出来的，
再与 `doctor --json` 的 `evidence` **逐行比集合**：

- `path.*`：条目数、重复组与富余、硬编码用户名（总数与机器级）、失效条目、空条目、含空格、
  reparse（种类 + 目标）、非 ASCII、相对路径、预算四档；
- `env.*`：持久变量总数、重名（排除 Windows 默认变量）、`path-literal` 两个方向、
  目标不存在（先判"是不是列表"再判"像不像绝对路径"）、名字含空格；
- `system.*` 与 WSL：提权、Developer Mode、长路径三个值各自独立读一遍；Lxss 子键的
  `BasePath` 剥掉 `\\?\` 后与 `%LOCALAPPDATA%` 比；
- `tool.*`：从 `crates/core/src/detect/spec.rs` **现场解析**出 12 个工具 / 23 个命令名
  （ID 白名单与来源白名单同样从 `crates/core/src/doctor.rs` 现场提取，**不抄一份**），
  自己走一遍 `PATH` 解析出每个命令的赢家，再与 `tool.multiple-active` 的证据逐行比；
  幽灵记录的每个卸载键都回 ARP 里查一遍 `InstallLocation`。

**这一票没有让机器上任何东西发生变化**：跑之前与跑之后，两个作用域的 `Path`（原文 + 类型 +
SHA256）、`%LOCALAPPDATA%\tuoen`（含 `shims` 一层）、store 清单、Lxss 子键、持久变量个数
逐项相同（工件末尾的 `user_path_untouched=True machine_path_untouched=True tuoen_dir_untouched=True`）。
