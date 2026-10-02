# L1-12 验收：捕获（`tuoen capture` → `tuoen.d/`）

- 票据：[#12](https://github.com/Muelsysel/tuoen/issues/12)（L1 捕获）
- 规范：`docs/specs/L1-dev-state.md`（issue #10）
- 决策：`docs/DESIGN.md` §1.12（决策 84–90）、§1.13（决策 91–94，本票验收期新增）
- 机器工件：[`L1-12-capture-run.txt`](L1-12-capture-run.txt)（221 行，逐段原样粘贴，没有手写数字）
- 可重跑脚本：`scripts/acceptance-L1-12.ps1`
  - `pwsh -File scripts/acceptance-L1-12.ps1` → **67 项通过 / 0 项失败 / 1 项跳过，exit 0**
  - `pwsh -File scripts/acceptance-L1-12.ps1 -SelfTest` → 67 / **1**，**exit 1**（证明它真的会报失败）
  - 跳过的 1 项是 `-SkipBuild` 下的 release 构建（二进制已经存在，摘要行里明说是跳过而不是通过）

---

## 1 逐条验收

### 1.1 票据「验收标准」三条

| # | 标准 | 判据与证据 | 结论 |
|---|---|---|---|
| ① | `cargo test --workspace` 全绿，clippy `-D warnings` 干净 | `729 passed / 0 failed`；`cargo clippy --workspace --all-targets -- -D warnings` exit 0；`cargo fmt --all --check` exit 0 | 通过 |
| ② | 真机跑一次 `tuoen capture`，把 `[budget]` 段与**重复条目计数**贴进评论，与取证报告对照 | `[budget]` 见 §2；重复条目 **18 条**（`dup_index > 0` 的行数），与脚本独立数出来的"48 条非空 − 30 个不同值 = 18 条富余"**逐数对上** | 通过（票面预期数字已过时，见 §2） |
| ③ | 确认产出文件里不含任何密钥材料，贴出跳过清单作为证据 | `skipped.toml` 一条：`ARK_API_KEY`（`credential-named`）；把它的**整值 / 前 8 字符 / 后 4 字符**拿去搜 30 个产出文件，命中 **0**；`env.toml` 里没有它；成功载荷里没有 `reason` 键 | 通过 |

### 1.2 票据「必须有的用例」逐条对应

| 票据要求的用例 | 落在哪条测试上 | 结果 |
|---|---|---|
| 幂等：捕获两次 → 除 `captured_at` 外逐字节相同 | `capture::test_support::tests::capturing_twice_with_a_pinned_timestamp_is_byte_identical`、`the_timestamp_really_is_the_only_difference_between_two_runs`、`capture::collect::path::tests::the_same_machine_captures_byte_identically_twice`、`capture::collect::env::tests::capturing_twice_is_byte_identical`、`capture::collect::wsl::tests::capturing_twice_is_byte_identical` | 6/6 |
| 密钥排除：fixture 放一个 `glpat-` 形状的 token，断言它不出现在任何输出文件，且跳过清单里有条目 | `capture::secrets::tests::a_gitlab_pat_is_recognized_by_its_prefix`、`capture::collect::env::tests::a_credential_is_skipped_and_never_reaches_any_file`、`the_skipped_variable_is_absent_from_env_toml`、`crates/cli/tests/capture_contract.rs::no_written_file_contains_anything_that_looks_like_a_credential` | 4/4 |
| `process-only` 条目（只在进程 `PATH` 里、不在注册表里）必须被捕获 | `capture::collect::path::tests::a_process_only_directory_is_captured_with_no_registry_type`、`a_process_only_index_is_its_position_in_the_process_path`、`empty_process_entries_do_not_shift_a_process_only_index` | 3/3 |
| `scope` 正确性：同名变量同时在用户级与机器级 → 两条都捕获且 `scope` 不同 | `capture::collect::env::tests::the_same_variable_in_two_scopes_is_two_rows_not_one` | 1/1 |
| `has_username`：机器级与用户级各一条 | `capture::collect::path::tests::a_hardcoded_username_is_reported_in_both_scopes_and_variables_are_not` | 1/1 |
| `reparse`：junction 与 symlink 各一条 fixture | `capture::collect::path::tests::reparse_shapes_are_recorded_with_their_targets` | 1/1 |
| `env` 变量名含空格：往返不丢 | `capture::collect::env::tests::a_variable_name_with_a_space_round_trips`（真机上就是 `IntelliJ IDEA`） | 1/1 |
| `--only path` → 只产出 `path.toml` 与 `schema.toml` | `capture::test_support::tests::only_path_produces_exactly_two_files`、`crates/cli/tests/capture_contract.rs::only_path_writes_only_the_path_file_and_says_so_in_the_schema`、`only_is_repeatable_and_the_schema_sections_are_sorted` | 3/3 |
| `--out` 到临时目录 → 不污染工作目录 | `crates/cli/tests/capture_contract.rs::a_run_with_out_in_a_temp_dir_does_not_touch_the_working_directory`、`a_default_run_writes_exactly_the_six_promised_files` | 2/2 |

### 1.3 票据「硬性约束」

| 约束 | 判据 | 结论 |
|---|---|---|
| 测试不得读真实 `HKCU\Environment` / `HKLM` / 真实 `PATH` / 用户真实安装目录 | 所有采集用例都走 `CaptureFixture`（假注册表 + 假文件系统 + 注入的运行器）；唯一碰真机的测试是 `crates/platform` 里那条要 `TUOEN_REAL_MACHINE_TESTS=1` 才跑的老用例 | 通过 |
| 测试不得访问真实网络 | 探测只经过注入的 `FakeProcessRunner`（`capture::collect::tools::tests::probing_goes_through_the_injected_runner_only`） | 通过 |
| fixture 必须是固定装置，不依赖开发机实时状态 | 每一条用例自己声明机器形状；`machine()` 那份固定装置是一张"一张表多个人读"的缩影 | 通过 |
| `captured_at` 必须可从比较中排除 | `without_timestamp`（按行首删 `captured_at` / `"capturedAt"`），三条用例钉住它只碰时间戳 | 通过 |
| **不写入除 `tuoen.d/` 之外的任何位置** | 验收脚本 §8：跑完两个作用域的 `Path`（原文 + 类型 + SHA256）逐字节相同、`%LOCALAPPDATA%\tuoen` 与 store 清单逐项相同、`%TEMP%` 根上没有留下 `tuoen.d`；§1 另有一条**结构断言**：捕获路径里的文件**没有出现**任何 `set_value` / `delete_value` / `broadcast_environment_change` / `create_junction` / `remove_junction` 调用 | 通过 |
| 不触碰 nvm4w 的符号链接 / 环境变量 / `settings.txt` | 捕获只读；验收脚本对 `C:\nvm4w\nodejs` 只做 `inspect`（记录成 `symlink-dir` + `link_target`，见 §4） | 通过 |

---

## 2 真机数字：与票面预期对照

票据里的预期数字来自**取证期**（2026-09 末），机器在那之后装过东西。两个都贴出来：

| 量 | 票面预期（取证期） | 真机实测（2026-10-03） | 差异说明 |
|---|---|---|---|
| `raw_user_chars` | 725 | **773** | 取证之后用户级 `PATH` 长过（`C:\Users\Muelsyse\.local\toolchains\mingw64\bin` 等） |
| `raw_machine_chars` | 978 | **1007** | 同上（机器级多了 Oracle / HALCON 相关条目） |
| `effective_chars` | 1781 | **1781** | 一致 |
| 重复条目 | 10 组 | **18 条** | 计数口径不同（见下）+ 机器变过 |

**重复条目那 18 条是怎么数出来的**：`PATH` 两段原文合起来 49 段，其中 1 段是空的，剩下 48 段去重（去尾部反斜杠 + 忽略大小写）后是 30 个不同的值，**富余 18 条** —— 这正是 `dup_index > 0` 的行数。两套量法（Rust 侧与 PowerShell 侧）逐数对上，而它们**不共享任何代码**。

票面说的"10 组"是**重复组数**（一个值出现 2 次以上算一组），与"富余条数"是两个量：本机 18 条富余分布在 12 个组里。两个数都在 `--json` 之外的地方出现过，**口径必须说清楚，否则"10 vs 18"看起来像有人算错了**。

`[budget]` 段（票据要求贴的那一段，原文见工件 §C1）：

```toml
[budget]
raw_user_chars = 773
raw_machine_chars = 1007
effective_chars = 1781
cliff = 8191
remaining = 6410
level = "ok"
```

`effective_chars = 1781` 与 `1007 + 1 + 773` 相等，也与验收脚本独立重建的"新终端口径"长度相等 —— 本机两段 `Path` 的原文里**都没有 `%VAR%`**（都是 `REG_SZ`），所以展开是恒等操作。这一条是**可证伪的等式**，不是"看着差不多"。

---

## 3 真机验收抓出来的问题（三条，都已修）

### 3.1 空条目被记成"目录不存在"（决策 91）

真机 HKLM 的 `Path` 第 21 段是空的（`;;`）。第一版让空串走正常分支，磁盘对 `""` 当然答"不存在"，于是它被记成 `exists = "no"`：真机上 8 条 `no` 里有 1 条是它。

**"这里缺一个目录"与"这里本来就什么都没有"是两件事。** 修法：空条目 `exists` 恒为 `unknown`、**不问磁盘**，形状记在新的 `empty` 字段上。判"失效条目"的判据变成 `!empty && exists == "no"` —— 真机上从 8 条变成 **7 条**，与 PowerShell 独立数出来的 7 条一致。

不修会怎样：票据 #13（`doctor`）照 `exists == no` 数失效条目，就会多报一条**永远修不好**的"坏条目"。

### 3.2 `dup_index` 的语义与票据不符（决策 92）

票据写的是"同值条目里的序号，0 = 首次出现"，而实现做成了"不同的值依次编号"（`A B C C D → 0 1 2 2 3`）。于是"从没重复过"的值也会拿到非零号，`dup_index > 0` 的行数变成"不同值的个数 − 1"—— 真机上会印 **32** 而不是 18，而票据要的正是"重复条目计数"。

修法：`let slot = seen.entry(key).or_insert(0); let index = *slot; *slot += 1; index` —— 从没出现过的值恒为 0。两条用例钉住（`the_dup_counter_is_the_occurrence_ordinal`、`the_same_value_twice_gets_zero_then_one`）。

**这一条是"实现与文档都自洽、但两者都与票据不一致"** —— 三条平行定义里只有票据算数。

### 3.3 列表不是一条路径（决策 92）

`env.toml` 的 `target_exists` 规则里写着"`;` 列表不是路径"，但代码只判前缀：`Path` 与 `PSModulePath` 的第一个字符就是 `C`，于是**整条 `;` 串**被拿去问磁盘，答案恒为"不存在"。真机上 4 条 `target_exists = "no"` 里有 3 条是这么来的（`Path` 两份 + `PSModulePath`），而"`Path` 指向的目录不存在"是一句会直接误导 `doctor` 的假话。

修法：`is_a_list(value) = value.contains(';')` 排在 `looks_like_absolute_path` **前面**；列表型变量的 `target` 也不记（那是 `path.toml` 的事）。真机上 `no` 从 4 条变成 **1 条**，而剩下的那一条（`HALCONROOT` 指向已经卸载的 HALCON）才是**真的**发现。

### 3.4 顺带记一个我自己的假检查

第一次核对 `reparse` 分布时，我用 `^reparse = "symlink"$` 去数，得到"symlink 0 条"，而真机上是 `symlink-dir`（2 条）。差点被当成"采集器没认出 nvm4w 的符号链接"报出去 —— **检查写错枚举拼法会造出一个假的差异**，而它与"代码错了"长得一模一样。这条与 L0 的决策 50、78 是同一类教训的第三次出现。

---

## 4 这台机器上真实读到的形状（值得留下的）

| 形状 | 真机证据 |
|---|---|
| Oracle JDK 的 `javapath` / `java8path` | `javapath` 是**失效条目**（`exists = "no"`），`java8path` 是 **junction**（`reparse = "junction"`，`link_target = …\java8path_target_1783390`）—— 安装器改了名却把老条目留在 `PATH` 里 |
| `C:\Software\tool` vs `C:\Software\tools` | `tool` 出现 3 次、**全都不存在**，而 `tools` 存在 —— 票据 #15 说的"疑似拼写错误**只报告不自动纠错**"在本机有一个真实实例 |
| nvm4w | `C:\nvm4w\nodejs` 在机器级出现 2 次，`reparse = "symlink-dir"`、`link_target = C:\Users\Muelsyse\AppData\Local\nvm\v24.19.0` |
| docker-desktop 的 `\\?\` 前缀 | `base_path = '\\?\C:\Users\Muelsyse\AppData\Local\Docker\wsl\main'`，而 `non_standard_path = false`、`vhdx_exists = "yes"`、100663296 字节 —— **不剥前缀就会同时报出两个假问题** |
| Arch-Linux-current | `base_path = 'C:\linux\Arch-Linux-current'`，`non_standard_path = true`，vhdx 1548746752 字节 |
| `ARK_API_KEY` | 用户级、46 字符、名字里有 `KEY` → 跳过清单里一条 `credential-named`。**取证报告里"环境变量里零密钥"是错的**（它只扫了 68 个进程变量，没扫持久作用域） |
| 空条目 | HKLM `Path` 第 21 段 |
| `reg_type` 两种都有 | `path.toml` 49 条全是 `sz`（所以 `expanded == raw`）；`env.toml` 里 `expand-sz` 12 条、`sz` 20 条 —— 决策 86 在真机上真的分开了（例：`ComSpec` 展开成 `C:\WINDOWS\system32\cmd.exe`，而 `Path` 一个字节都不展开） |

其它真机读数（`--json` 原文见工件 §B）：

- `tools.toml` 27 条，置信度分布 `executable 10 / registered-missing 6 / registered 4 / alias-ghost 2 / directory-only 2 / manager-owned 2 / managed 1`；来源分布 `path-resolution 11 / registry-arp 10 / filesystem-scan 2 / manager 2 / app-paths 1 / tuoen 1`；`reproducible = false` 12 条。
- `owner` 分布：`system 15 / third-party 3 / unknown 31`（`unknown` 是默认值，不是失败）。
- `env.toml` 32 行（用户级 13 + 机器级 19），而两个作用域的持久变量一共 **33** 个 —— 差的那 1 个就是被跳过的 `ARK_API_KEY`。**分母必须说出来**："写进去 32"与"机器上有 33"是两个数。
- `--no-version`：版本行从 15 降到 3，而**仍然有 3 条**（maven 的目录名、nvm4w 的布局、tuoen 自己的安装记录）—— 票据文字说的"版本会是空的"不准确，见 §5。

---

## 5 诚实未覆盖

1. **只在一台机器上跑过**（Windows 11 专业版 build 10.0.26200，单用户）。多用户、多语言的机器没测过。
2. **`app-exec-alias` 在真机的 `PATH` 上没有出现**（`WindowsApps` 那一条不在 `PATH` 上），所以那一档只在固定装置里验证过。
3. **`expand-sz` 的 `PATH` 没在真机上出现过**：本机两段 `Path` 都是 `REG_SZ`，所以 `expanded` 的真机行为只有"不展开"这一半被验证过；"展开"那一半靠固定装置（`expanded_follows_the_registry_type_not_the_value`）。
4. **`has_vars = true` 的条目在真机上 0 条**：`%VAR%` 保留、`exists = "unknown"` 那条路只在固定装置里走过。`%NOPE%\bin` 这类条目在真机上还没遇到。
5. **`target_exists = "unknown"` 在真机上 0 条**（`yes 16 / no 1 / not-a-path 15`）—— 展开不出来的那一态同样只有固定装置覆盖。
6. **密钥扫描只覆盖环境变量**：`.m2/settings.xml` 里的明文 GitLab PAT（用户 `zhangpengzhan`）这一票**不扫**，是票据 #17 的事。本票只把跳过清单的格式定下来。
7. **`--only` 的组合只测了单数**（`--only path` 等四种各一次，以及 `--only path --only env` 的顺序保持）；没有测"同一 section 重复给两次"的真机行为（单测里有 `only_is_repeatable_and_keeps_the_order_the_user_gave`）。
8. **幂等只在同一状态上测过**：跨状态的两份快照本来就该不同，本票不涉及。
9. **`capture` 在 `PATH` 逼近 8191 时的行为没在真机上制造过**（本机 1781 / 8191，`level = "ok"`）。预算档位的边界值只在单测里验证（6143 / 6144 / 7371 / 7372 / 8191 / 8192）。
10. **`tools.toml` 的 27 条里，我们只对 node 做过"下载—安装—跑起来"的端到端**（L0 的 #9）；其它 26 条是**探测**结果，没有逐条手工核对过。
11. **`skipped.toml` 的判据是启发式**：它认得出 `glpat-` / `ghp_` / `AKIA` 这类形状与"名字像凭据"两类，但**认不出一个长得像普通字符串的凭据**。跳过清单说的是"我们看见了、故意没写"，不是"其余都安全"。

---

## 6 门禁与复现

```
cargo test --workspace                                  729 passed / 0 failed
cargo clippy --workspace --all-targets -- -D warnings   exit 0
cargo fmt --all --check                                 exit 0

pwsh -File scripts/acceptance-L1-12.ps1                 67 通过 / 0 失败 / 1 跳过，exit 0
pwsh -File scripts/acceptance-L1-12.ps1 -SelfTest       67 通过 / 1 失败，exit 1
```

复现真机读数（**不需要先构建**，脚本自己会构建；`-SkipBuild` 可以跳过）：

```powershell
# 只读验收：跑 capture、独立核对、幂等、--only、跳过清单、收尾核对
pwsh -File scripts/acceptance-L1-12.ps1

# 手工重跑一次"新终端口径"的捕获（子进程的 PATH 从注册表重建，不写注册表）
$machine = (Get-Item 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Environment').GetValue('Path','','DoNotExpandEnvironmentNames')
$user    = (Get-Item 'HKCU:\Environment').GetValue('Path','','DoNotExpandEnvironmentNames')
$env:Path = [Environment]::ExpandEnvironmentVariables("$machine;$user")
& target\release\tuoen.exe capture --out $env:TEMP\mydump --json | ConvertFrom-Json | ForEach-Object { $_.data.path }
```

**验收这一票没有让机器上任何东西发生变化**：跑之前与跑之后，两个作用域的 `Path`（原文 + 类型 + SHA256）、`%LOCALAPPDATA%\tuoen` 的清单、store 的清单逐项相同（工件 §I）。真机端到端那一票（#9）为了让"新终端里生效"成为证据**真的写过一次用户级 `PATH`**，这一票不需要 —— `capture` 是只读命令。
