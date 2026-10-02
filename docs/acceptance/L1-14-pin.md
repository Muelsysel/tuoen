# L1-14 验收：项目级 pin（`tuoen.toml` / `tuoen.lock` / `shell` / `trust` / `auto`）

这一票的招牌承诺是"**在项目里 pin 一套版本，进这个目录就拿到它**"。它的失效形态不是崩溃，
而是**用户以为切了版本、其实没切** —— 所以这一票的验收核心是**端到端的那一条**：

```
tuoen shell --exec 'node -e "console.log(process.execPath)"'
→ C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.19.0\node.exe
```

打印出来的是**我们 store 里的那个 `node.exe`**，不是 nvm4w 的、也不是系统 `PATH` 上任何一个。

- 验收脚本：`scripts/acceptance-L1-14.ps1`（**70 条检查**，`-SelfTest` 必须 exit 1）
- 机器工件：`docs/acceptance/L1-14-pin-run.txt`
- 设计决策：`docs/DESIGN.md` §1.4（14–17，骨架）+ §1.16（113–125，实现与验收期）

## 1 逐条验收

### 1.1 票据「验收标准」四条

| 验收标准 | 证据 |
| --- | --- |
| `cargo test --workspace` 全绿 | **974 passed / 0 failed**（本票新增：core `pin` 99 条 + CLI `pin_contract` 25 条） |
| clippy `-D warnings` 干净 | `cargo clippy --workspace --all-targets -- -D warnings` → exit 0；`cargo fmt --all --check` → exit 0 |
| 真机演示一次（临时目录 + `tuoen.toml` + `shell --exec "node -v"` + junction 前后） | §2.1 |
| 未信任目录不自动切换的证据 | §2.2 |
| `tuoen trust --list` 的输出 | §2.3 |

### 1.2 票据「必须有的用例」逐条对应

| 票据点名 | 落点 |
| --- | --- |
| `shell` 不翻转全局 junction | `pin_contract::the_dry_runs_touch_neither_the_store_nor_any_junction`（跑前后隔离家目录清单逐项相同）+ 验收脚本 §7（真机上 `C:\nvm4w\nodejs` 与 store `node\current` 的目标前后各贴一次） |
| 子进程 `PATH` **前置**了 pin 的版本目录 | `pin_contract::the_child_shell_really_gets_the_prepended_path_and_the_depth`（`--exec "echo %PATH%"` 的**第一条**必须是前置目录；深度必须是 `1`）+ 真机 `execPath`（§2.1） |
| 遮蔽检测：机器级条目遮蔽前置目录 → stderr 警告且含条目名 | core `shell::shadowed_command_names_the_winner_and_its_raw_entry` / `winner_inside_a_prepended_dir_is_not_a_warning` / `empty_prepended_dir_reports_every_command`；CLI `a_command_that_did_not_take_effect_is_warned_about_on_stderr`（断言每条 warning 都有 `winner`/`entry` 两个键） |
| 指纹：相同通过 / 改一个字符拒绝 / **CRLF ↔ LF 通过** | core `crlf_and_lf_are_the_same_content` / `one_changed_character_is_stale` / `bom_and_trailing_whitespace_are_content`；验收脚本 §4（真机 CRLF 副本指纹与 LF 版**逐字相同**） |
| 未信任目录 → 不自动切换（断言环境未变） | CLI `auto_refuses_an_untrusted_directory_and_writes_nothing`；验收脚本 §4（退出 1、`error.code == "untrusted"`、项目目录清单未变） |
| `--revoke` 后 → 不再自动切换 | CLI + 验收脚本 §4（`action == "revoked"` → `auto` 回到 `untrusted`，且 `--list` 里那条消失） |
| `tuoen.toml` 与 `tuoen.lock` 不一致 → 明确告知 | core `mismatches_covers_the_three_cases_116`（spec 不同 / 只在 toml / 只在 lock）；CLI `a_lock_that_disagrees_with_the_pin_refuses_to_start`；验收脚本 §5（`lock-mismatch` + 消息含 `tuoen lock`） |
| pin 未安装的版本 → 提示含下一步命令 | core `MissingTool::next_command`；CLI + 验收脚本 §6（消息含 `tuoen install node@99`） |
| 信任清单写入**可注入路径**，不碰真实 `%APPDATA%` | `TrustFile::at_path` / `at_default_location()`（读 `%APPDATA%`）；契约测试与验收脚本都把 `APPDATA` 注入成临时目录，并断言**真实的** `%APPDATA%\tuoen` 跑前后逐项相同 |
| 不得依赖真实交互式 shell | core 的 `ShellLauncher` trait（core 里没有一行启动进程的代码）+ `--exec`（非交互）+ `--dry-run`（连 spawn 都不做） |

### 1.3 票据「硬性约束」

- **测试不得读真实的 `HKCU\Environment` / 真实 `PATH` / 真实 `%APPDATA%\tuoen`**：core 的
  99 条用例全部走 `pin/test_support.rs` 的假装置（`TempDir` / `FakeDirs` / `detected_tool`），
  没有一条读真机；CLI 的 25 条走 `IsolatedHome`（`APPDATA`/`LOCALAPPDATA`/`USERPROFILE` 全部指向临时目录），
  并显式断言"真实的那个清单跑前后逐项相同"。
- **不得翻转真实的 junction**：`crates/cli/src/shell_cmd.rs` 里没有 `repoint_junction` /
  `activate` / `fs::write` 到 store 的调用（`shell`/`auto` 只读 store）；验收脚本 §7 用 junction 目标的前后对照做证据。
- **不得触碰 nvm4w 的符号链接 / 环境变量 / `settings.txt`**：全票没有写 `C:\nvm4w` 的代码；
  验收脚本把它当**只读的对照组**（前后各读一次目标）。
- **测试不得访问真实网络**：`pin` 一族的代码里没有 `Transport` / `curl`；`resolve` 只读 store 与检测结果。
- **不得依赖真实交互式 shell**：见上表最后一条。

### 1.4 票据「明确不做」

- **不实现企业策略的执行**：`trust.toml` 里有 `[policy] source = "user"`，系统级
  `%PROGRAMDATA%\tuoen\policy.toml` **只写在文档里**；core 里没有任何一行去读它（决策 122）。
- **不实现 `tuoen use` 的替代**：`shell` 不翻转 junction（那仍是 L0 的 `use`）。
- **不做 shell 钩子**：`auto` 不进任何 shell profile；默认关闭（决策 121）。
- **不探测全局包**：那是 #17。

## 2 真机演示

### 2.1 临时项目：pin `node = "24"`，然后进这个目录

`%APPDATA%` 注入成临时目录（于是信任清单不落真实位置），其余一切照旧。

```text
### junction 目标（跑之前）
  C:\nvm4w\nodejs        -> C:\Users\Muelsyse\AppData\Local\nvm\v24.19.0
  store node\current     -> C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.19.0

### tuoen shell --exec "node -v"
  v24.19.0

### tuoen shell --exec 'node -e "console.log(process.execPath)"'
  C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.19.0\node.exe

### junction 目标（跑之后）
  C:\nvm4w\nodejs        -> C:\Users\Muelsyse\AppData\Local\nvm\v24.19.0
  store node\current     -> C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.19.0
```

**两个 junction 的目标前后逐字相同** —— `shell` 只改了**那个子进程**的环境块。
`execPath` 那一行是关键：本机上 `node -v` 无论用哪一份都是 `v24.19.0`（nvm4w 的当前版本
与我们的 store 恰好同号，这是 L0-09 就记下的坑：**版本号不是证据**），只有 `process.execPath`
能证明子进程跑的是**我们**那份。

`--dry-run --json` 的计划与它逐项一致（脚本 §2 独立核对）：

```
prepend[0]  = C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.19.0
tools[0]    = {name: node, spec: 24, version: 24.19.0, source: tuoen, path: <同上>}
lockWritten = false          ← `--dry-run` 一个字节都不写
```

### 2.2 未信任目录：`auto` 拒绝执行（默认关闭）

```text
$ tuoen auto --dry-run
tuoen: `C:\Users\Muelsyse\AppData\Local\Temp\tuoen-l114-demo\proj` 不在信任清单里，所以 `tuoen auto` **不会**自动应用它的 pin。
进入陌生仓库就自动改工具链版本是安全漏洞（恶意仓库可以 pin 一个带后门的「Node 版本」），所以自动切换默认关闭。
这个目录有 `tuoen.toml`，跑 `tuoen trust` 可启用自动切换。
退出码=1
```

### 2.3 `tuoen trust` 与 `tuoen trust --list`

```text
$ tuoen trust
已信任：C:\Users\Muelsyse\AppData\Local\Temp\tuoen-l114-demo\proj
指纹：sha256:988aed93d1b50559e2cfcf11f37feb518c299f8726457a532b97c1e1a143b4cc
清单：C:\Users\Muelsyse\AppData\Local\Temp\tuoen-l114-demo\appdata\tuoen\trust.toml

从这一刻起，在这个目录（以及指纹没变的它）里 `tuoen auto` 会应用 pin。
`tuoen.toml` 改一个字符就会让指纹对不上，那时 `auto` 会拒绝执行并让你重新 trust。

$ tuoen trust --list
tuoen trust —— 信任清单

清单：C:\Users\Muelsyse\AppData\Local\Temp\tuoen-l114-demo\appdata\tuoen\trust.toml

目录                                                       现在          指纹
C:\Users\Muelsyse\AppData\Local\Temp\tuoen-l114-demo\proj  trusted       sha256:988aed93d1b50559…

1 条 `trusted`：指纹与信任时一致，`tuoen auto` 会应用这个目录的 pin。

摘掉一条：`tuoen trust --revoke <目录>`。
```

信任之后 `auto --dry-run` 通过，计划里多一行 `信任：trusted`，前置目录与 `shell` **逐项相同**
（契约测试断言两者的 `data` 除 `trust` 键外逐字节相同 —— 它们共用同一个计划构造器）。

## 3 验收期抓出来的问题（五条，都已修）

### 3.1 `[project]` 的形状：代码、文档、用例三处自洽，而用户会照票据写（决策 124）

第一版把项目名实现成顶层 `project = "我的项目"`（一个字符串），模块文档与用例都照这个写，
`cargo test` 全绿。而票据 §1 的样例是：

```toml
[project]
name = "my-project"
```

真机验收**第一次跑就红了**：`pin-parse`，`project` 必须是字符串、现在读到的是一张表。
改成表形状（段内键名写错也报错），并补 `the_project_name_lives_in_a_table_not_in_a_bare_string`
同时钉住"字符串形状被**明确拒绝**"（不是静默当成"没有项目名"）。
这是决策 92 那条"字段的语义以票据为准，不以代码与自己的文档自洽为准"的**第五次**。

### 3.2 `cmd.exe /C` 的载荷转义：一句"输出为空、退出码 0"的假成功（决策 125）

真机演示第一次跑出来的是**空**：

```text
$ tuoen shell --exec 'node -e "console.log(process.execPath)"'
（什么都没有）  退出码 0
$ tuoen shell --exec "node -v"
v24.19.0        ← 不含引号时照常工作
```

根因：`std::process::Command::arg` 按 MSVCRT 规则把含引号的参数转义成 `\"`，而 `cmd.exe`
**不认**这种转义 —— 它看到的是字面的反斜杠。修法是只对 `cmd.exe /C` 的载荷用
`CommandExt::raw_arg`（原样交给 cmd 自己的解析器），PowerShell 那条继续走普通 `arg`。
判据是 `--exec 'echo "hi"'` 必须印出 `"hi"` 且不许出现 `\"`（`exec_keeps_the_quotes_that_cmd_needs`，
在旧实现下必红）。

**这条的教训是"判据的形状"**：`--exec "node -v"` 能跑，`--exec "exit 7"` 也能跑 ——
只看"子进程起没起来、退出码对不对"是发现不了这个 bug 的。要发现它，判据必须**穿过引号**。

### 3.3 两句"看起来完全合理的错话"（复核时脚本化抓到的）

复核时把新增代码里所有 `tuoen <命令>` 的出现抽出来跟真实子命令表对了一遍：

| 位置 | 原话 | 问题 |
| --- | --- | --- |
| `crates/core/src/pin.rs` 的 `MissingPin` | "先在这个目录跑 `tuoen pin` 建一份" | `tuoen pin` **不存在**（`unrecognized subcommand 'pin'`，退出码 2）。而这是用户第一次跑 `tuoen shell` 时看到的第一句话 |
| 同文件 `UnknownTool` | "跑 `tuoen list` 看看现在有哪些工具" | `tuoen list` 存在但**答非所问**：它只列**我们自己管的**工具；"我们认识哪些工具"是 `tuoen catalog list` |

两处都改了，并把用例改成**否定判据**（消息里不许出现这两个串）。这是"验收脚本自己写的每一句话
也要能被独立复核"的延伸：**产品自己说的每一句话也要能被独立复核**，而复核的办法是把它跟
真实命令表对一遍 —— 不是读一遍觉得通顺。

### 3.4 子代理自查出来的两条输出质量 bug

- **表格按字符数补空格**：中文表头的显示宽度与字符数不同，整张表错开两列 → 改成按显示宽度补，
  列宽从数据里算（`path-resolution` 15 列会把路径列推歪），单测钉住。
- **遮蔽警告印成「抢到的是 `A` 里的 `A`」**：core 的 `winner`（归一化文本）与 `entry`
  （`PATH` 里的原文）指同一个目录时重复了 → 抽成纯函数 `warning_line()`，两条单测钉住
  （含"没有赢家"那一态是**另一句话**）。

## 4 诚实未覆盖

- **只有一台机器，且只有 `node` 是我们装的**。端到端演示（`execPath`）用的是 node；
  `java` / `python` 这类第三方 pin 会走**版本探测**（`probe_versions: true`），真机上没有
  演示过它们的 `shell --exec`。
- **`--shell powershell` 只验到"起得来 + 退出码透传"**（`--exec "exit 4"` → 4），没有验
  前置 `PATH` 在 PowerShell 子进程里的效果。
- **Ctrl-C 未测**：交互式子 shell 的 Ctrl-C 发给整个进程组，`tuoen` 自己也会收到 ——
  最坏情况下 `tuoen` 先退出、子进程继续跑、退出码传不回来。这条限制写在
  `spawn_inherit` 的文档里，装控制台处理器是 `crates/shim` 转发器那件事（决策 56）。
- **信任清单的路径比较是文本的**（绝对化 + 忽略大小写 + 忽略尾部分隔符），**不做 realpath**：
  同一个目录的两种拼法（junction / `..` / 8.3 短名）会算成两条。这是刻意的（realpath 会引入
  对磁盘状态的依赖），但没在真机上构造过这种形状。
- **`lock` 里的 `hash` 只有我们自己的安装才有**（来自 `InstallRecord.sha256`）；第三方
  安装没有哈希，字段缺席（不是空字符串）。L3 的离线 bundle 要靠它，那时第三方来源的缺口会显出来。
- **指纹的归一化只做 CRLF→LF**（决策 117）：BOM、末尾换行、空白都算内容。真机上验过
  CRLF↔LF，没验过 BOM。
- **`auto` 的"自动"是用户显式调用**：没有 shell 钩子（票据明确不做），所以"进目录就自动切"
  这件事在 L1 还不存在 —— 存在的是"进目录后 `tuoen auto` 会切，而 `tuoen shell` 永远会切"。
- **企业策略只预留格式**：`%PROGRAMDATA%\tuoen\policy.toml` 没有读它的代码。
- **`tuoen.lock` 的解析失败路径**（手写坏锁）只被 core 的用例覆盖，真机没构造过。

## 5 门禁与复现

```powershell
$env:Path = "$env:USERPROFILE\.cargo\bin;C:\Users\Muelsyse\.local\toolchains\mingw64\bin;$env:Path"

cargo fmt --all --check                                        # exit 0
cargo test --workspace                                         # 974 passed / 0 failed
cargo clippy --workspace --all-targets -- -D warnings          # exit 0

# 真机验收（70 条检查；注入 %APPDATA%，不碰真实信任清单）
pwsh -File scripts/acceptance-L1-14.ps1                        # exit 0，PASS
pwsh -File scripts/acceptance-L1-14.ps1 -SelfTest              # 必须 exit 1
```

脚本 §1–§6 的每个数字都是**它自己**算出来的，再与产品的输出对照：

- 它自己从 `%LOCALAPPDATA%\tuoen\store\node\versions` 读出候选版本、自己按"数字成分前缀"算出
  `24` 命中的是 `24.19.0`，再与 `--dry-run --json` 的 `prepend[0]` / `tools[0]` 逐项比；
- 它自己读两个 junction 的目标（`C:\nvm4w\nodejs` 与 store 的 `node\current`）、
  真实的 `%APPDATA%\tuoen` 与 `%LOCALAPPDATA%\tuoen` 清单，跑前跑后各一次；
- 它把 CRLF 副本的指纹与 LF 版的指纹**逐字对照**（而不是"看它没报错"）。

**这一票没有让机器上任何东西发生变化**：真实 `%APPDATA%\tuoen`、真实 store（含 `shims`）、
两个 junction 的目标逐项相同；项目目录里多出来的只有 `tuoen.lock`（那是决策 116 说的：
`shell` 首次解析时写出锁），以及注入的临时 `%APPDATA%` 里的 `trust.toml`。
