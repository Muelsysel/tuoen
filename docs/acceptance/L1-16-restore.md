# 票据 #16 `tuoen restore` —— 真机验收

**结论：PASS。** `scripts/acceptance-L1-16.ps1` 真机跑出
`checks_passed=125 checks_failed=0 checks_skipped=0 verdict=PASS`，退出码 0；
`-SelfTest` 跑出 `checks_passed=124 checks_failed=1 verdict=FAIL`，退出码 **1**（证明它会报失败）。

工件（全部可复现，命令在 §6）：

| 文件 | 是什么 |
|---|---|
| `docs/acceptance/L1-16-restore-run.txt` | 那一轮真机验收的**原始输出**（154 行）+ `-SelfTest` 的结论块 |
| `docs/acceptance/L1-16-restore-plan.txt` | 真机快照的**完整恢复计划**（人类输出逐字，票据要求贴进 issue 的那份） |
| `scripts/acceptance-L1-16.ps1` | 验收脚本本身（1028 行 / 126 条检查） |
| `crates/core/tests/restore_plan.rs` + `fixtures/restore/**` | 22 条**不碰这台机器**的集成用例（自造快照 + 假机器根） |
| `crates/cli/tests/restore_contract.rs` | 19 条 CLI 契约用例（只读真实注册表 + 断言跑前跑后逐字节相同） |

**这一轮几乎没有写东西**，唯一的例外是 §9：用 `--only env --apply` 真写**一个新建的**用户级
环境变量，验证"计划 → 真的落盘 → 新终端看得到 → 再跑一次是空的 → 删掉之后整个
`HKCU\Environment` 逐字回到原样"。票据禁止的是"在开发机上真的执行一次**完整** restore" ——
那一条没有做，也没有被绕过（见 §5 的诚实未覆盖）。

---

## 1. 逐条验收

### 1.1 票据"必须实现的"七条

| # | 票据要求 | 判据 | 结果 |
|---|---|---|---|
| 1 | plan → diff → apply 统一引擎；`--dry-run` 与真实执行**同一套代码路径**；`--only <section>`；用户确认后才 apply | 默认形态与显式 `--dry-run` 的 `--json` **逐字节相同**；`--apply` 与 `--dry-run` 同时给 → clap 退出 2；`--only path` 时其余三节 `skipped` + `not-selected` | ✅ 决策 150；用例 `the_default_form_and_the_explicit_dry_run_are_byte_identical` |
| 2 | `tools` 用 L0 安装引擎装缺失的工具，**不接管**第三方管理器 | 真机快照 `missing = 0`、`installed = 27`、无 `install` 动作；`manual_actions` 里 `third-party-manager` **按管理器去重** = `nvm4w` + `uv` | ✅ 决策 154/163 |
| 3 | `path` 复用上一票的 diff + 重建 + 选择性应用 | 与 `path diff` 对**同一份快照**给出同一组分类数字（`keep 25 · add 0 · remove 0 · move 0 · fix 24 · caseOnly 0`）—— 两条命令、两套装配 | ✅ 决策 151/158 |
| 4 | `env` 写缺失变量（用户级默认；机器级产 `requires-elevation`）；**不写任何密钥材料** | 真机快照 `present = 32`、无 `set-user`；被跳过的 `ARK_API_KEY` 只以 `skipped-secret` 出现；拿本机真实值的前 8 字符与全长比对，输出里**不出现** | ✅ 决策 152/153 |
| 5 | `wsl` 只报告，不自动导入 vhdx | `same = 2`、`effective` 恒空、`note = report-only`、apply 时 `report-only` 且一个字节不写 | ✅ 决策 159 |
| 6 | `manual_actions` 五类**公开契约**，`credential-reconfigure` **只写意图** | 五类 code 全在允许集合内、每条 `subject`/`detail`/`remediation` 全 ASCII；真机 3 条（`nvm4w` / `uv` / `env:ARK_API_KEY`） | ✅ 决策 152/153/160/163 |
| 7 | 幂等；部分失败不留半成品；无网络时至少完成元数据部分 | 默认形态连跑两次 `--json` 逐字节相同；`--only env --apply` **真跑两次**：第二次 `wrote = false`、`apply.sections = []`、`summary.wouldChange = 0`；plan 永不需要网络（`needsNetwork` 是计划里的数据） | ✅ 决策 155/156/157/164 |

### 1.2 票据"必须有的用例"逐条对照

| 票据点名的用例 | 落在哪 |
|---|---|
| 幂等：`restore` 两次 → 第二次 plan 为空、输出"无变更"、退出码 0 | `restore_contract.rs` + 验收脚本 §5（逐字节相同）与 §9（**真的 apply 两次**） |
| `--dry-run` 与真实执行的 plan 逐条相同 | `the_default_form_and_the_explicit_dry_run_are_byte_identical`（CLI 边界逐字节）+ `plan()` 是纯函数（决策 150） |
| `--only path` → 断言 `tools` / `env` 未被改动 | `only_path_touches_only_path` + `an_empty_plan_touches_nothing`（纯函数 `sections_to_apply`）+ 验收脚本 §6 断言其余三节 `skipped`/`not-selected` 且 `actions` 为空 |
| `manual_actions` 五类各一个用例 | `every_manual_action_class_appears_and_managers_are_deduped` + `a_licence_blocked_tool_names_the_concrete_reason` + `a_skipped_credential_is_a_todo_without_any_material` + `a_machine_scope_variable_asks_for_elevation_instead_of_doing_it` + `a_third_party_managed_tool_has_no_writing_action` |
| `credential-reconfigure` 断言输出里不含 token 的任何片段 | `no_plan_field_carries_any_fragment_of_the_canary_token`（查**所有 8 字符窗口**，比"长度 ≥ 8 的子串"更严）+ `a_credential_never_leaks_a_material`（CLI 侧，`--json` 与人类输出都比）+ 验收脚本 §3（拿**本机真实** `ARK_API_KEY` 的前 8 字符与全长比） |
| `licence-blocked`：不可再分发的制品 → 被拒**且原因具体** | `a_licence_blocked_tool_names_the_concrete_reason`（fixture 的 `oracle-jdk`）+ 验收脚本 §7（合成快照里追加一行 `oracle-jdk`，断言 `detail` 逐字 = `oracle-jdk-redistribution-not-permitted`，且**没有** `install` 动作） |
| `third-party-manager`：断言假注册表后端 / 假 `settings.txt` 未被写 | 在 plan 那一层写这条是**恒真断言**（`plan()` 签名里没有注册表），所以真证据分两处：CLI 契约测试的 `registry_snapshot` / `assert_registry_unchanged`（真实注册表逐字节未变），以及验收脚本 §4/§10 的**真机红线**：`NVM_HOME` / `NVM_SYMLINK` 在两个作用域里的原文与类型、`%APPDATA%\nvm\settings.txt` 的哈希、`C:\nvm4w\nodejs` 这个 junction 的**指向**，整轮跑前跑后逐字相同 |
| 部分失败：不留半成品、可重入 | "不留半个目录"这一半由 L0 原语承担：`crates/archive/tests/evil_archives.rs:481` 的 `a_failure_after_extraction_still_leaves_nothing_behind`（配合 `:71` 的 `assert_no_staging_left`，在 10 处被调用）；"可重入"由 `a_half_finished_apply_plans_only_what_is_left` 承担（fixture `halfway-local`：上一次做到一半 → 重跑只列剩下的） |
| 无网络：传输层返回网络错误 → 元数据部分仍完成、且指明哪些步骤需要网络 | `needsNetwork` 是**计划里的数据**（决策 157），`tools` 一节在合成快照里 `status = needs-network`；plan 阶段零网络 |
| 空快照 → 明确报错 | 验收脚本 §6：空目录 → 退出 1 + `empty-snapshot`；`a_truly_empty_directory_is_also_an_empty_snapshot` + `a_missing_directory_is_an_io_error_not_an_empty_snapshot` |

### 1.3 票据"验收标准"四条

1. `cargo test --workspace` 全绿、clippy `-D warnings` 干净 → **1233 passed / 0 failed**、clippy exit 0、fmt exit 0（§6）。
2. **在作者本机上真实跑一次 `tuoen restore --dry-run`**（输入是本机自己 `capture` 出来的 `tuoen.d/`）→ §3，完整计划在 `L1-16-restore-plan.txt`。
3. 贴出 `manual_actions` 的真实输出（预期至少含凭据重配的意图，且不含任何材料）→ §3.3。
4. 幂等性证据（连跑两次，第二次为空）→ §3.4（**真的 apply 两次**，不只是计划）。

**"接近空"这个结果本身**：四个 section 全 `no-change`、`has_changes() == false`、`summary` 六个计数器 `noChange=4` 其余全 0。**但它不是"什么都没做"** —— `manual_actions` 有 3 条（§3.3）。这正是决策 161 想要的分工：`status` 回答"你会不会动我的机器"，`manual_actions` 回答"我要做什么"。

---

## 2. 真机事实与两个分母

**这一节的每个数字都由脚本自己从注册表/磁盘/快照文件数出来**，不与产品自己的输出对照
（AGENTS.md"会写报告的代码"规矩 4）。

| 量 | 值 |
|---|---|
| `HKCU\Environment\Path` | **773** 字符 · `REG_SZ` · sha16 `a12fc582e90513a3` |
| `HKLM\…\Path` | **1007** 字符 · `REG_SZ` |
| "新终端口径"的合并 PATH | **1781** 字符（= 1007 + 1 + 773，脚本从注册表不展开读后拼出来，供子进程用） |
| 快照 `tools.toml` | **27** 行（`reproducible = true` 15 / `false` 12；管理器 `nvm4w`、`uv`） |
| 快照 `env.toml` | **32** 行（user 13 / machine 19） |
| 快照 `path.toml` | **49** 行（machine 33 + user 16） |
| 快照 `wsl.toml` | 2 个发行版（`Arch-Linux-current`、`docker-desktop`） |
| 快照 `skipped.toml` | 1 条（`env` / `user` / `ARK_API_KEY` / `credential-named`） |
| 计划的 path 分类 | `keep 25 · add 0 · remove 0 · move 0 · fix 24 · caseOnly 0`（与 `path diff` 逐项相同） |
| 计划的 `manual_actions` | 3 条：`third-party-manager:nvm4w`、`third-party-manager:uv`、`credential-reconfigure:env:ARK_API_KEY` |
| nvm4w 的红线 | `NVM_HOME` / `NVM_SYMLINK` 两个作用域都是 `REG_EXPAND_SZ`；`settings.txt` sha16 `02f3156cdf563d43`；junction → `C:\Users\Muelsyse\AppData\Local\nvm\v24.19.0` |

### 2.1 两个分母都必须写出来：`tools` 是 **27** 行，不是 29 行

同一个量在别处出现过两次，两个都对：

- **29 行**（`tools.toml`，`reproducible` 17 / 12）—— 那是开发 shell 里 `capture` 出来的：
  那条 PATH 上多了 `%USERPROFILE%\.cargo\bin`，于是 `cargo` 与 `rustc` 各多出一行
  `path-resolution`（`cargo 1.99.0` 的来源）。
- **27 行**（本票验收用的那一份）—— 捕获时子进程的 PATH 被换成**从注册表重建的"新终端口径"**
  （1781 字符）。`.cargo\bin` **不在** `HKCU\Environment\Path` 里（它是 process-only），
  所以一个真正新开的终端看到的也是 27 行。

**这不是"哪个数过期了"，而是两个不同的口径。** 验收脚本用的是后者，因为票据要的是
"还原到**本机**" —— 而"本机"的判据必须与一个真实新终端一致。这条同时是 `restore` 的一个
真陷阱（`pathdiff-cli` 在取证时踩过一次）：**`capture` 与 `restore` 必须在同一个 PATH 下跑**，
否则"对着本机自己的快照"会报出假的 `missing` 与假的 `needs-network`。

---

## 3. 真机演示

### 3.1 默认形态 = 只出计划（完整人类输出见 `L1-16-restore-plan.txt`）

```
恢复计划 —— 快照：…\real\tuoen.d
模式：只出计划（**什么都没有写**）。要真的做：`tuoen restore … --apply`

[tools] 工具 · 无变更
  行数：extra 0 · installed 27 · missing 0 · third-party 0 · unsupported 0
[path] PATH · 无变更
  行数：add 0 · caseOnly 0 · fix 24 · keep 25 · move 0 · remove 0
  当前用户名：Muelsyse（来自进程环境 USERPROFILE）—— 同一份快照在不同用户名下类别会不同
  说明：fix 类（本机自身的健康问题：重复 / 失效 / 空条目 / 用户名）**默认不选中** —— 要一起应用加 `--with-fix`
[env] 环境变量 · 无变更
  行数：missing-machine 0 · missing-user 0 · present 32 · secret-skipped 1
  · [env:ARK_API_KEY] 凭据，跳过 ARK_API_KEY（scope=user）
[wsl] WSL · 无变更
  行数：extra 0 · missing 0 · path-differs 0 · same 2
  说明：只报告，**不写**

摘要：4 个 section —— 无变更 4 · 会变更 0 · 需要提权 0 · 需要网络 0 · 不支持 0 · 跳过 0
这份快照要写的东西：**没有** —— 本机已经和它一致。
```

### 3.2 `--json`（整份载荷一行，1477 字符，逐字）

```json
{"schemaVersion":1,"command":"restore.plan","ok":true,"data":{"manualActions":[{"code":"third-party-manager","detail":"nvm4w","remediation":"use-the-manager","subject":"nvm4w"},{"code":"third-party-manager","detail":"uv","remediation":"use-the-manager","subject":"uv"},{"code":"credential-reconfigure","detail":"credential-named","remediation":"reconfigure-manually","subject":"env:ARK_API_KEY"}],"sections":[{"actions":[],"counts":{"effective":{},"rows":{"extra":0,"installed":27,"missing":0,"third-party":0,"unsupported":0}},"id":"tools","needsNetwork":false,"requiresElevation":false,"status":"no-change"},{"actions":[],"counts":{"effective":{},"rows":{"add":0,"caseOnly":0,"fix":24,"keep":25,"move":0,"remove":0}},"id":"path","needsNetwork":false,"note":"fix-not-selected","requiresElevation":false,"status":"no-change"},{"actions":[{"detail":"scope=user","id":"env:ARK_API_KEY","kind":"skipped-secret","subject":"ARK_API_KEY"}],"counts":{"effective":{},"rows":{"missing-machine":0,"missing-user":0,"present":32,"secret-skipped":1}},"id":"env","needsNetwork":false,"requiresElevation":false,"status":"no-change"},{"actions":[],"counts":{"effective":{},"rows":{"extra":0,"missing":0,"path-differs":0,"same":2}},"id":"wsl","needsNetwork":false,"note":"report-only","requiresElevation":false,"status":"no-change"}],"snapshot":"…\\real\\tuoen.d","summary":{"manualActions":3,"needsNetwork":0,"noChange":4,"requiresElevation":0,"sections":4,"skipped":0,"unsupported":0,"wouldChange":0}}}
```

（`snapshot` 那一格在真实载荷里是**用户敲的那个路径原样**；这里为了排版省略了前缀。）

### 3.3 `manual_actions` 的真实输出（票据要求贴的那份）

```
人工待办（3 条）—— **无变更不等于你什么都不用做**：
  · [第三方版本管理器] nvm4w：由第三方版本管理器 `nvm4w` 管 —— tuoen **不接管**它：改它的符号链接要管理员，而且两个管理器会抢同一个 reparse point。
    怎么办：用它自己的命令升级（tuoen 不接管）
  · [第三方版本管理器] uv：同上
  · [凭据要自己重配] env:ARK_API_KEY：变量名里有 `KEY` / `TOKEN` / `SECRET` 这类词，且值够长、不像路径 —— 无法排除它是凭据，所以**它没有进快照**（材料从不进快照）。
    怎么办：自己把这个凭据重新配一遍
```

**材料没泄漏**：脚本用**本机真实** `ARK_API_KEY`（46 字符）的**前 8 字符**与**全长**去比
`--json` 与人类输出，两处都没有命中（值本身从未被打印）。

**与票据预期的一处差异，如实说明**：票据写"预期至少含 `credential-reconfigure` 的
**git 凭据**意图"（`git:https://git.seawayos.com:8443`）。本票的真机证据是**环境变量**的凭据意图
（`env:ARK_API_KEY`）—— 因为 `capture` 目前只采集**环境变量**，`~/.gitconfig` / `~/.m2/settings.xml`
这类**配置文件**属于票据 #17（globals+configs）。`credential-reconfigure` 的 `subject` 契约
（协议 + host + 路径）在 #17 落地时会第一次出现；本票证明的是同一条红线（**只写意图、绝不写材料**）。

### 3.4 幂等：**真的 apply 两次**

| 步骤 | 结果 |
|---|---|
| 探针快照（只有 `env.toml`，多一条本机没有的用户级变量） | `env` → `would-change`、`effective.set-user = 1` |
| 先看计划（不带 `--apply`） | 载荷里**没有** `apply` 键；注册表里那个变量**还不存在** |
| `restore … --only env --apply --json` | 退出 0；`apply.wrote = true`；`apply.sections = [{id:"env", outcome:"applied", wrote:true, broadcastReplies:0}]` |
| 从注册表**不展开**读回 | `restore-apply-probe` 逐字相等；类型 `REG_SZ`（值不含 `%`） |
| 一个**全新终端**（从注册表重建环境块） | 看得到它 |
| **再跑一次**同一个 apply | 退出 0；`apply.wrote = false`；**`apply.sections = []`**（这一轮什么都没考虑过）；计划的 `summary.wouldChange = 0`；值仍恰好一条 |
| 清理（可写子句柄，与自检**完全相同**的路径） | **整个 `HKCU\Environment`（14 个值名 + 类型 + 原文）与跑之前逐字相同** |
| 清理之后的新终端 | 又看不到它了 |

`broadcastReplies = 0` 是**本机的真实情况**（没有顶层窗口响应这次 `WM_SETTINGCHANGE`），
脚本只断言这个键**存在**（它只在真的广播过之后才出现），**不断言它 ≥ 1** —— 那会变成一条看机器
脸色的检查。

---

## 4. 验收期发现的问题

### 4.1 三个是**我的期望错了**，产品是对的（已写进决策）

| 我原本断言 | 真实结果 | 谁错 | 处理 |
|---|---|---|---|
| 合成快照里 `wsl` 是 `would-change` | `no-change` + `note = report-only`，但**有** `missing-distro` 动作 | **我错** | `wsl` 的动作一个字节都不落盘 → 决策 161 的 ⑤"有会写的动作"不成立 → 落到 ⑥。与 `path` 的 `fix-not-selected` 是**同一个形状**：有差异、什么都不写、note 说清楚。已补进决策 161 |
| `summary.wouldChange = 3` | `2` | **我错** | 同上（wsl 不进这一格）。六个计数器相加 = `sections` 这条不变量成立 |
| `--with-fix` 时 `path` 是 `would-change` | `requires-elevation` | **我错** | `fix` 里含**机器级**条目（本机 2 条），机器级只算不写 → 决策 161 的 ④ 先于 ⑤。这正是那条顺序存在的理由 |
| 第二次 apply 的 `apply.sections` 里 `env` 是 `no-change` | `apply.sections` 是**空数组** | **我错** | 它只列"这一轮真的考虑过"的节（`sections_to_apply`），空计划 → 空数组 —— 那正是票据要的"第二次为空"。已补进决策 164 |

### 4.2 五个是**脚本自己的坑**（全是"假的红"或"假的白"）

1. **`DirectoryInfo` 没有 `Length` 属性。** `Get-TreeListing` 里 `$_.Length` 在
   `Set-StrictMode -Latest` 下是**终止错误** → 脚本第 2 秒就死在"列出 store 目录"上。
   修法：按 `PSIsContainer` 分开取。
2. **`$Obj.PSObject.Properties.Name` 在空对象上会炸。** `effective` 在"没有要写的条目"时是
   `{}` —— 而**那是最正常的形态**。成员枚举在空集合上取 `.Name` 报
   `在此对象上找不到属性"Name"`。修法：对每个 `PSPropertyInfo` 逐个取 `Name`（`Prop`），
   并另加一个 `PropNames`。这条与 #15 的 `@($null).Count == 1` 同源：**空，不是"没有属性"**。
3. **`-join` 的优先级高于 `-eq`。** `(a, b, c) -join ',' -eq '0,0,0'` 先拼成字符串再比较，
   于是"计划里没有任何要写的东西"这条**正确**的形态被报成 FAIL。修法：加括号。
4. **`[int]` 不能转一个嵌套对象。** 我把 `counts.rows` 这个**对象**当数字转，直接抛
   `无法将类型"@{extra=0; …}"的值转换为"System.Int32"`。修法：取到具体键再转。
5. **`[0]` 索引落在可能为空的数组上。** 第二次 apply 的 `apply.sections` 为空 → `[0]` 抛
   `Index was outside the bounds of the array`，整条脚本**中止**（连 FAIL 都来不及打）。
   修法：加 `Sec`（找不到返回 `$null`），并把 13 处裸索引全部换掉 —— **产品哪天少报一节，
   应该打出一条 FAIL，而不是让脚本消失。**

### 4.3 一条与 #15 同源的教训

§9 的"还原机制自检"（先写一个一次性变量、确认出现、删掉、确认全环境块逐字回到原样）
**是照 AGENTS.md 第七条规矩做的**，而它这次**真的挡住了东西**：脚本第一次真跑时，
`Get-TreeListing` 的崩溃发生在 §1，也就是在 §9 写入之前 —— 如果没有"写入前先自证还原"
这条纪律，那次崩溃会发生在**写入之后**（§9 里任何一步抛异常都可能把探针变量留在注册表里）。
这正是决策 149 的复述：**"我们打算还原"和"还原真的能写"是两件事。**

---

## 5. 诚实未覆盖

1. **`--apply` 的三条真写通道里，只有 `env` 真的跑过。** 真机跑的是"一个新建的用户级变量"；
   `tools`（真下载安装）、`path`（真 `apply_rewrite` + 真广播）、以及 `--with-fix` 的机器级
   写路径**都没有在真机上执行**。能证明的是：它们与计划读同一份 `plan()`（纯函数），
   且"碰哪几节"由纯函数 `sections_to_apply` 决定（两条单测 + 一次变异测试）。
2. **没有提权**，所以 `requires-elevation` 的那条真写路径（机器级变量）在真机上从未执行 ——
   它只被计划与 `manual_actions` 覆盖。
3. **`broadcastReplies = 0`。** 本机这次广播没有任何顶层窗口响应，所以"广播真的送达了谁"
   没有被证明；被证明的是**广播被调用过**（那个键只在真广播之后出现）。
4. **网络路径未覆盖**：`tools` 的安装需要网络，本机没有真的装过任何东西；
   传输层失败 → 只跳过那几步（决策 157）只有单测。
5. **只在一台机器上验过**（Windows 11 + 非提权 + 已开长路径）。`NVM_HOME`/`NVM_SYMLINK`
   的"双重管理腐坏"、`Arch-Linux-current` 的非标准 vhdx 路径都是**这一台**的事实。
6. **"接近空"依赖 PATH 口径**（§2.1）：在带 `cargo` 的开发 shell 里捕获会多 2 行、多出
   假的 `missing`。这不是缺陷，但它是一个**必须写在文档里的前提**。
7. **`licence-blocked` 只在合成快照上验过**（真机快照里那 12 条不可复现行都已装，按决策 160
   它们不产生动作）；五类 `manual_actions` 的完整覆盖在 fixture 集成测试里，用的是自造数据。
8. **`-SelfTest` 只证明脚本"会报失败"**，不证明每一条检查都有牙齿。真正证明过敏感性的只有
   三处**变异测试**（`registry_snapshot` 护栏、`sections_to_apply` 的过滤条件、
   "默认形态 ≡ `--dry-run`"），分别由 CLI 侧在实现期做过。
9. **`credential-reconfigure` 的 git 形态（协议+host+路径）没有真机样本**（§3.3）——
   配置文件采集是 #17。
10. **`empty-snapshot` / `snapshot-io` / `snapshot-toml` 三个错误码的真机证据只覆盖前两个**
    （第三个在 CLI 侧有单测，验收脚本没有造坏 TOML）。

---

## 6. 门禁与复现

在提交前的最终树上（`cargo`/`rustc` 用 `$env:USERPROFILE\.cargo\bin`）：

```
cargo fmt --all --check                                   → exit 0
cargo test --workspace                                    → 1233 passed; 0 failed（#15 时 1107）
cargo clippy --workspace --all-targets -- -D warnings     → exit 0
pwsh -NoProfile -File scripts/acceptance-L1-16.ps1        → 125 PASS / 0 FAIL，exit 0
pwsh -NoProfile -File scripts/acceptance-L1-16.ps1 -SelfTest -SkipBuild
                                                          → 124 PASS / 1 FAIL（自检那条），exit 1
```

本轮测试数（逐二进制，来自 `cargo test --workspace` 的 `Running` / `test result` 行）：

| crate | 单测 | 集成测试 |
|---|---|---|
| `tuoen-core` | **447** | `restore_plan.rs` **22** |
| `tuoen-cli`（二进制 `tuoen`） | **180** | capture_contract 11 · catalog_contract 13 · cli_contract 10 · detect_contract 13 · doctor_contract 9 · manage_contract 23 · path_contract 16 · pathdiff_contract 20 · pin_contract 25 · real_machine_acceptance 5 · **restore_contract 19** · shim_contract 25 |
| `tuoen-platform` | **82** | path_contract 29 |
| `tuoen-archive` | **53** | evil_archives 26 |
| `tuoen-manifest` | **65** | — |
| `tuoen-download` | **47** | loopback 4 |
| `tuoen-shim` | **24** | shim_contract 19 |
| `tuoen-store` | **23** | store_ops 22 + 1（其余 7 行为空结果） |

**合计 1233 passed / 0 failed**（#15 时是 1107）。

**跑之前的安全网**：`HKCU\Environment\Path` 的原文与类型备份在
`%TEMP%\_l115_hkcu_backup.txt` / `.kind`（773 字符 / `REG_SZ` / sha16 `a12fc582e90513a3`），
每一轮跑完都复核过一致；§9 的探针变量在 `finally` 里删除并逐字比对**整个**环境块。
