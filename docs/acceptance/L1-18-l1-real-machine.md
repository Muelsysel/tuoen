# 票据 #18 —— L1 真机验收（capture → doctor → path diff → restore → 幂等）

- **机器**：作者本机（Windows、**非提权**、zh-CN），`node v24.19.0`（nvm4w 管的，tuoen 的 store 里另有一份 L0 时期装的同版本）。
- **日期**：2026-10-03。
- **结论**：`scripts/acceptance-L1-18.ps1` → **`checks_passed=83 checks_failed=0 checks_skipped=1 verdict=PASS`，exit 0**；
  `-SelfTest` → `83 passed / 1 failed`（那一条是故意失败的），**exit 1** ✓。
- **工件**：`L1-18-run.txt`（脚本逐行输出）、`L1-18-doctor.txt`（doctor 完整人类输出 201 行）、
  `L1-18-path-diff.txt`（path diff 完整人类输出）、`L1-18-restore-plan.txt`（restore 计划的人类输出）。

这一票**不写功能，只写证据**。它只写两种东西：`%TEMP%\tuoen-acceptance-L1-18\` 下的工件，
以及 §4 里**故意**写一个用户级环境变量（空计划上的"计划 vs 执行"对比是恒真的）。

---

## 1. 票据"必须做的事"逐条对照

### 1.1 完整走一遍（六步，退出码与真实耗时）

| 步骤 | 退出码 | 耗时 | 落盘 |
|---|---|---|---|
| `capture`（默认全量） | 0 | **3784 ms** | 八份文件（六个 section + `schema` + `skipped`） |
| `doctor` | 0（发现 7 条 error 也是 0） | **468 ms** | 人类输出 201 行 + `--json` 29 条发现 |
| `path diff <快照>` | 0 | **19 ms** | 人类输出 + `--json` 49 行 |
| `restore --dry-run` | 0 | **1458 ms** | 计划（四个 section 全 `no-change`） |
| `restore`（默认形态） | 0 | **1426 ms** | 与 `--dry-run` **逐字节相同** |
| `restore --apply`（空计划） | 0 | **1408 ms** | `apply = {sections: [], wrote: false}` |
| `restore`（第二次，验幂等） | 0 | 见 §4 | 非空计划那次：第二次变成 `no-change` |

（`restore --only env` 在非空计划上：计划 28 ms、真的写 219 ms、第二次 12 ms。以上数字逐字取自
`L1-18-run.txt` 的耗时块 —— 同一台机器上重复跑的差异在 ±10% 以内。）

### 1.2 票据表格里的每一项证据

| 证据 | 在哪 |
|---|---|
| `path.toml` 的 `[budget]` 段与重复条目计数 | §2.1（逐字） |
| `doctor` 的完整输出，逐条与取证报告对照 | `L1-18-doctor.txt` + §2.2 |
| `path diff` 的完整输出，逐条对照 | `L1-18-path-diff.txt` + §2.3 |
| `restore --dry-run` 的 plan（用本机自己 capture 的快照） | `L1-18-restore-plan.txt` A 段 + §2.4 |
| **`--dry-run` 与真实执行的对比，逐条列出差异（预期为空）** | §4（**真的比过**：空计划上两者逐字节相同；非空计划上"计划 1 条要写的动作 = 执行 1 节 applied"） |
| **未使用 `setx`** | §5（代码审查 + grep：产品源码 22 处提到、**0 处调用**） |
| **未触碰 nvm4w**（前后各一次：两个变量、junction 指向、settings.txt 哈希） | §6 |
| `HKCU\Environment\Path` 的变化（前后各一次字符数与哈希） | §6（**没变**：773 字符 / sha16 `a12fc582e90513a3`） |
| 幂等（第二次 `restore` 的输出，预期"无变更"） | §4.3 |
| `manual_actions` 真实输出，确认 `credential-reconfigure` **只含意图不含材料** | §2.4 |
| 实测数据（耗时 / PATH 长度变化 / shim 启动开销） | §7 |

---

## 2. 真机事实与三个能力的真实输出

### 2.1 `capture`：`[budget]` 段与重复条目（票据点名要贴）

```toml
[budget]
raw_user_chars = 773
raw_machine_chars = 1007
effective_chars = 1781
cliff = 8191
remaining = 6410
level = "ok"
```

**脚本自己从注册表算的**：`773 + 1 + 1007 = 1781` ✓ 与 `effective_chars` 逐字相同；
`cliff` 是 8191（`cmd.exe` 的悬崖，与 `setx` 的 1024 **不是同一个数字** —— 后者在代码里叫
`SETX_TRUNCATION`，并有单测断言两者不等）；`remaining = 8191 − 1781 = 6410` ✓ 自洽。

**重复条目：18 行，涉及 13 个不同的（归一化后的）值。** `dup_index` 的语义是**出现序号**：
`C:\Software\tool` 出现 3 次（machine#8 / machine#28 / user#11）→ `dup_index` = 0 / 1 / 2 ✓；
`C:\Program Files\dotnet` 与 `…\dotnet\`（带尾斜杠）算**同一个值** → 0 / 1 ✓。
脚本按自己实现的归一化（去尾分隔符 + 大小写不敏感）重算了一遍，**每个值的序号集合都是 0..n−1** ✓。

`path.toml` 共 49 行（48 条非空 + 1 条空条目），`empty = true` 标在那一条上 ✓；
`tools` 27 行 / `env` 32 行 / `wsl` 2 行 —— 与 #16 的真机口径一致（新 section 没有改动旧输出）✓。

### 2.2 `doctor`：29 条发现，逐条核对**没有一条是假的**

```
错误 7 · 警告 13 · 提示 9
规模：PATH 条目 49 · 环境变量 32 · 工具 27 · WSL 发行版 2 · shim 0
```

19 类 id：`env.duplicated-scope` · `env.missing-target` · `env.name-with-spaces` ·
`env.path-literal` · `path.duplicate` · `path.empty-entry` · `path.missing` · `path.reparse` ·
`path.spaces` · `path.username-hardcoded` · `system.developer-mode` · `system.elevated` ·
`system.long-paths` · `system.wsl-nonstandard-path` · `tool.ghost` · `tool.global-prefix-inside-version-dir` ·
`tool.multi-manager` · `tool.multiple-active` · `tool.unmanaged-directory`

逐条抽查（**我按自己的知识核对，不是照抄产品的话**）：

| 发现 | 我的核对 |
|---|---|
| `env.duplicated-scope` ×2（`NVM_HOME`/`NVM_SYMLINK`） | 真的：两个作用域都有（nvm4w 的安装器两边都写）✓ 而且**不是** `Path`/`TEMP` 那种平台设计（#13 收窄过这条判据）✓ |
| `path.duplicate`：**12 组、18 条富余** | 与脚本自己数的"18 行 `dup_index > 0`"**逐字相同** ✓ |
| `path.missing`：7 条 | 抽查 `C:\Program Files (x86)\Common Files\Oracle\Java\javapath`、`C:\Software\tool`（×3）、Python 3.11 的两条 —— 都真的不在 ✓ |
| `path.spaces`：14/48 = **29%** | 与 `docs/DESIGN.md` §2 里那条平台事实（29%）一致 ✓ |
| `path.username-hardcoded`：12 条（其中 2 条机器级） | 真的：`C:\Users\Muelsyse\…` 硬编码 ✓ |
| `tool.global-prefix-inside-version-dir` | 真的：`C:\nvm4w\nodejs` → `…\nvm\v24.19.0`（symlink-dir）✓ |
| `tool.multi-manager`（node：nvm4w + tuoen） | 真的 ✓ |
| `tool.multiple-active`（java：`java8path` vs `JDK8\bin`；python：WindowsApps vs Python312） | 真的 ✓ 这两条是**这份报告里最有用的两条** |
| `tool.ghost` ×4（dotnet ×2 / java / python / wsl） | 真的：注册表卸载键说装过、路径不在 ✓ 而且 `path=<placeholder>` + 独立 `uninstall-key={GUID}` —— **给「人」看的占位符没有漏进 evidence**（#13 的那条教训）✓ |
| `system.developer-mode` = absent / `system.elevated` = false / `system.long-paths` = true | 三条都真的 ✓（"不存在 ≠ 关着"这条三态规矩守住了）✓ |

**没有一条误报。** 唯一值得记的噪声：5 条 `env.path-literal`（`NVM_HOME`/`NVM_SYMLINK` ×2 + `OneDrive`）
说"类型是 `REG_EXPAND_SZ` 但值里没有 `%`" —— 那是 nvm4w / OneDrive 安装器留下的，
**消息自己也写着"不一定是错的，但类型与内容不一致，值得看一眼"** ✓ 所以它不是假话，是**一条信息量偏低的 warn**。
`evidence` 逐行断言纯 ASCII ✓、`message` 不进 `--json` ✓、`counts` 与实际分布自洽 ✓。

### 2.3 `path diff`：同一台机器上 capture 再 diff，必须"什么都不用加、什么都不用删"

```
逐条 49 行（本机侧的行 + 只有目标侧的行）：
  keep 25 · add 0 · remove 0 · move 0 · fix 24 · case-only 0
```

- **`add` / `remove` / `move` / `case-only` 全是 0** —— 这是"capture 与 diff 说的是同一台机器"的硬证据 ✓
  （脚本里这是一条断言，不是观察）。
- `keep + fix = 49` ✓ 每一行都被归了类；`fix 24` 全是**本机自己的**健康问题（重复 / 失效 / 空条目 / 用户名）。
- `snapshot` 字段是**用户敲的那个路径原样** ✓；`currentUsername` 与脚本自己从 `USERPROFILE` 取的一致 ✓。
- 拼写疑似 5 条，**只报告、永不纠错**（决策 24）✓。

### 2.4 `restore`：计划（本机 = 接近空的计划）

```
摘要：4 个 section —— 无变更 4 · 会变更 0 · 需要提权 0 · 需要网络 0 · 不支持 0 · 跳过 0
这份快照要写的东西：**没有** —— 本机已经和它一致。

注意：这份快照里还有 **globals · configs** —— L1 的 `restore` **不还原**它们（只捕获）。
  这不是「没看见」：它们的内容在快照里（`globals.toml` / `configs.toml`）。……

人工待办（3 条）—— **无变更不等于你什么都不用做**：
  · [第三方版本管理器] nvm4w：由第三方版本管理器 `nvm4w` 管 —— tuoen **不接管**它……
  · [第三方版本管理器] uv：……
  · [凭据要自己重配] env:ARK_API_KEY：……**它没有进快照**（材料从不进快照）。怎么办：自己把这个凭据重新配一遍
```

`manualActions` 的 JSON 逐字：`code=third-party-manager subject=nvm4w`、`code=third-party-manager subject=uv`、
`code=credential-reconfigure subject=env:ARK_API_KEY` ✓ —— **凭据那条只有变量名，没有值**；
脚本另外断言了"计划载荷里没有 `ARK_API_KEY` 的**值**"（值不打印）✓，而且先确认了本机**真的有**这个变量
（否则那条断言是空真）✓。

---

## 3. `--dry-run` 与真实执行：**真的比过**

### 3.1 空计划上：两者逐字节相同，而且**一个字节都没写**

`restore --dry-run --json` 与 `restore --json` 的载荷**逐字节相同**（决策 150：同一套计划代码，
`--dry-run` 只是把"我只要计划"说出来）✓。在空计划上 `--apply`：`apply.wrote = false`、
`apply.sections = []` ✓ —— 而"没写"不是听它说的，是**跑前跑后逐字比过**：
两个作用域的 `Path`、整个 `HKCU\Environment`（14 个值）、nvm4w 四件、`store/` 与 `cache/` 树 —— 全部相同 ✓。

### 3.2 非空计划上：故意加一个本机没有的变量

空计划上的对比是恒真的，所以脚本**故意**让计划非空：复制快照、往里追加一行
`[[var]] TUOEN_L1_ACCEPT_PROBE / scope = "user" / value_raw = "l118-<时间戳>"`（值不是凭据 ✓），
然后：

| 步骤 | 真实输出 |
|---|---|
| 计划 | `env` 节 `status = would-change`；`actions` 两条：`{kind: "set-user", subject: "TUOEN_L1_ACCEPT_PROBE"}` 与 `{kind: "skipped-secret", subject: "ARK_API_KEY"}` |
| 真的执行 | `apply = {"sections":[{"broadcastReplies":0,"id":"env","outcome":"applied","wrote":true}],"wrote":true}` |
| 逐条对比 | 计划里 **1 条要写的**（`set-user`）= 执行了 **1 节 applied** ✓ **差异为空** ✓（`skipped-secret` 是只报告的动作，按决策 161 不算写入 ✓） |
| 落盘 | 注册表里读回的值与快照逐字相同 ✓ 类型 `REG_SZ` ✓（值里没有 `%`，所以不该用 `REG_EXPAND_SZ` ✓） |
| 新终端 | **从注册表重建环境**的新进程看到 `l118-<时间戳>` ✓（在当前 shell 里再 `echo` 一次证明不了任何事） |
| 幂等 | 第二次计划变成 `no-change` ✓ |
| 清理 | 删掉探针变量后，整个 `HKCU\Environment` 与跑之前**逐字相同**（值名 + 类型 + 原文）✓ |

**这一票唯一一次写机器**，写之前做了三件事：整个 `HKCU\Environment` 备份到
`%TEMP%\tuoen-acceptance-L1-18\hkcu-environment-backup.json`（含类型与原文）、
印出**可粘贴的恢复命令**、并且**先自证还原能用**（把备份原样写回一次，断言逐字未变 ✓）——
这是 AGENTS.md「会改用户系统状态的代码」第七条的形状。清理包在 `try/finally` 里，
所以**哪怕中途抛了，探针变量也一定会被删掉** ✓。

---

## 4. 未使用 `setx`（代码审查 + grep）

- **产品源码（含测试）：22 处提到 `setx`，0 处调用。** 逐条看：文档注释（`cli.rs` / `path_cmd.rs` /
  `path_diff_cmd.rs` / `path.rs`）、一条人类输出里的警告句（`path_view.rs:667`）、
  一个**具名常量** `pub const SETX_TRUNCATION: usize = 1024;`（`lib.rs:85`）、
  两条断言"这个数字与 `PATH_CLIFF_CMD` **不是同一个**"（`lib.rs:95/96`）、一条测试消息（`path_contract.rs:223`）。
  `Command::new` / `args(` 里一次都没有 ✓。
- **脚本（`scripts/*.ps1`）：48 处提到，0 处是会执行的语句**（判据：去掉注释后 `setx` 后面跟着参数的样子）。
  守卫自己必须能写出它禁止的东西，所以"提到"不算命中 ✓。
- **反向验证**：脚本往临时文件里塞一条真的 `& setx TUOEN_L118_PROBE 1`，判据**必须命中 1 条** ✓（它命中了）。
- 全仓库 82 处提到（产品源码与测试 22 / 脚本 48 / 其余在文档与根目录文件里），**没有一处是可执行的调用**。

---

## 5. 未触碰 nvm4w（跑前跑后逐字相同）

| 事实 | 跑之前 | 跑之后 |
|---|---|---|
| `NVM_HOME`（user + machine，类型 + 原文） | `ExpandString\|C:\Users\Muelsyse\AppData\Local\nvm` | **相同** |
| `NVM_SYMLINK`（user + machine） | `ExpandString\|C:\nvm4w\nodejs` | **相同** |
| `C:\nvm4w\nodejs` 的 junction 指向 | `C:\Users\Muelsyse\AppData\Local\nvm\v24.19.0` | **相同** |
| `%NVM_HOME%\settings.txt` | 66 B，sha256 `02f3156cdf563d43…` | **相同** |
| `HKCU\Environment\Path` | 773 字符，`REG_SZ`，sha16 `a12fc582e90513a3` | **相同** |
| `HKLM\…\Path` | 1007 字符，`REG_SZ`，sha16 `d0b5a0db7c9e2a6b` | **相同** |
| `store/` 与 `%APPDATA%\tuoen\cache` 树 | — | **逐项相同** |

（票据写的是 `C:\nvm4w\settings.txt`，真机上那个文件在 `%NVM_HOME%\settings.txt` —— 以机器为准 ✓。）

---

## 6. 实测数据

| 项 | 数值 |
|---|---|
| `capture`（全量，`--json`） | 3784 ms（第二次 3810 ms） |
| `doctor` | 468 ms（`--json` 448 ms） |
| `path diff` | 19 ms（人类）/ 15 ms（`--json`） |
| `restore`（计划，四个 section） | 1458（`--dry-run`）/ 1426（默认）ms |
| `restore --apply`（空计划） | 1408 ms |
| `restore --only env`（计划 / 真的写 / 第二次） | 28 / 219 / 12 ms |
| `PATH` 长度 | 机器级 1007 + 用户级 773 = **1781 字符**（跑前跑后相同），非空条目 48 条 |
| shim 启动开销 | `shims\node.exe --version` 中位 **27 ms** vs 真身 `store\node\current\node.exe` 中位 **18 ms** → **+9 ms**（一次进程启动的量级）；输出逐字相同（`v24.19.0`） |
| shim 第一次跑 | 111 ms（刚写出来的 `.exe` 会被 Defender 扫一遍；更早一次跑到过 1932 ms）—— **冷启动那一次不能当基准** |
| 安装耗时 | **本票没有装任何东西**：`store` 里已经有 L0 时期装的 `node 24.19.0`（`tuoen list` 确认），shim 直接拿它测；本机 27 个工具的 `missing` 是 0，所以 `restore` 的安装路径这一票没跑（见 §8） |

---

## 7. 验收期发现的问题

### 7.1 三条是**我的期望错了**，产品是对的

1. **`dup_index` 按归一化后的值编号，不是按 `raw` 原文。** 我第一版按 `raw` 分组，于是
   `C:\Program Files\dotnet\`（带尾斜杠、`dup_index = 1`）看起来像错 —— 其实它与
   `C:\Program Files\dotnet` 是同一个值 ✓。**机器数据自己给出了答案**（`C:\Software\tool` 三次 → 0/1/2）。
2. **`actions` 里同时有"要写的"和"只报告的"。** 我第一版断言"恰好 1 条动作"，实际是 2 条
   （`set-user` + `skipped-secret`）—— 决策 161 早就写了"report-only 的动作不算写入" ✓。
3. **`shim remove` 收的是命令名，不是工具 id。** 我第一版 `shim remove node` 只删掉 `node.exe`，
   留下 `npm.exe` / `npx.exe` / `corepack.exe`，于是"shim 目录回到原样"这条红了 —— 帮助里写得很清楚
   （`<NAMES>...` = 命令名）✓。**观察（不判为 bug）**：`shim add <工具>` 与 `shim remove <命令名>`
   的参数形状不对称，第一次用很容易踩；本票的脚本改成"从 shim 目录里自己数出四条名字再删" ✓。

### 7.2 一条是脚本自己的排序疏忽

期望字符串写成 `corepack,npm,npx,node`，而 `Sort-Object` 给的是 `corepack,node,npm,npx` ✓ ——
与 #15 的"两边都要先排序"同源，只是这次我自己没排。

### 7.3 `#21` 在真机上复现（不改代码，只记录）

`tuoen shim remove tuoen-definitely-not-a-shim` → **退出码 1**（#21 的期望是幂等成功 0）。
这一票只记录行为，**不动代码**（票据明令）✓。

---

## 8. 诚实结论：L1 的四个能力，哪些真的能用、哪些还有洞

**① 捕获（`capture`）—— 能用。**
八份文件、49/32/27/2 行、两次捕获除时间戳外**逐字节相同**、`[budget]` 与注册表自算逐字一致、
跳过清单 11 条逐条带具体原因。洞：`--no-version` 下 `unknown` 与"没有这个运行时"合流（#17 已记，
决策 172 的取舍）；"缓存目录绝不走进去"靠构造而非用例（#17 已记）。

**② 诊断（`doctor`）—— 能用。**
29 条发现逐条核对**没有一条是假的**，`evidence` 纯 ASCII，`counts` 自洽，退出码语义正确
（"发现 7 条 error"仍然是 0 —— 严重度是数据，不是失败）。洞：5 条 `env.path-literal` 信息量偏低
（nvm4w/OneDrive 安装器的类型/内容不一致）；`doctor` 只看环境与工具，**不看配置文件内容**
（`configs` 的凭据判断在 `capture` 那边，两者不交叉）。

**③ PATH 重建（`path diff` / `path apply`）—— diff 能用；apply 这一票没真跑。**
`diff` 六类 + 理由 + 拼写只报告；同机 capture 再 diff 的 `add/remove/move/case-only` 全 0 = 自洽。
**洞（诚实缺口）**：本机的计划是 `keep 25 · fix 24`，而 `restore` 默认**不含** `fix`（决策 151），
所以这一票没有任何"真的写 `PATH`"的动作 —— 那条证据在 #15 的真机验收里（真跑过 `--only add` /
`--only fix` 并还原，含那次"写进去了、还原不了"的事故）。另外 #20 仍然开着：`path remove` 拒绝摘掉
shim 目录、而且没有退路。

**④ 还原（`restore`）—— 能用，但这一票只覆盖了 env 一节的真实写入。**
空计划上 `--apply` 一个字节没写（逐字证明）；非空计划上真的写了一个用户级变量、**新进程看得到**、
第二次变成 `no-change`、删掉后整个 `HKCU\Environment` 逐字回到原样。**洞**：`tools` 节的安装路径
这一票没跑（本机 27 个工具 `missing = 0`，没有可装的）；`path` 节的写入没跑（同上，`fix` 默认不选）；
`wsl` 只报告（设计如此）；机器级改动与凭据**只算不做**（决策 136 / 147，设计如此）。
另外 #19 仍然开着：下载源失败要等 150 秒才换源 —— 它会直接拖慢 `restore` 的 `tools` 节。

**已开着的三个洞**：#19（换源 150 s）、#20（`path remove` 拒绝摘 shim 目录）、#21（`shim remove`
对不存在的名字报失败，**本票在真机上复现**）。这三个都不是这一票能改的（写入范围只有 `docs/`）。

---

## 9. 诚实未覆盖

1. **只有一台机器**（决策 158 的同一条限度）：所有真机数字都来自作者本机。
2. **`tools` 节的安装路径没跑**：本机 27 个工具 `missing = 0` ⇒ 没有可装的（§8）。
3. **`path` 节真的写入没跑**：本机计划 `add/remove/move = 0`，`fix 24` 默认不选中 ⇒ 这一票没有
   "真的重建 `PATH`"的证据（证据在 #15）。
4. **机器级改动与凭据重配只有"意图"**：`manualActions` 里的 3 条永远不会被自动执行（设计如此）。
5. **`doctor --no-probe` 这条分支没跑**（默认开探测；关掉之后 `tool.global-prefix-inside-version-dir`
   应该是"没看"而不是"没有"—— 只有单测覆盖）。
6. **`restore --with-fix` 没跑**（它会真的修本机的 24 条 `fix`，属于"改状态"的一步，这一票不做）。
7. **`--json` 的字节稳定性只在"同一台机器两次"上验过**，没有跨版本兼容性证据（`schemaVersion` 仍是 1）。
8. **shim 的启动开销只有一台机器、五次采样的中位数**，而且第一次必然被 Defender 污染（§6）。

---

## 10. 门禁与复现

```
cargo fmt --all --check                                → exit 0
cargo test --workspace --no-fail-fast                  → 1307 passed / 0 failed
cargo clippy --workspace --all-targets -- -D warnings   → exit 0
pwsh scripts/acceptance-L1-18.ps1                       → 83 passed / 0 failed / 1 skipped，exit 0
pwsh scripts/acceptance-L1-18.ps1 -SelfTest             → 83 passed / 1 failed（故意），exit 1
```

复现前把 `cargo` 与 `dlltool` 放回 `PATH`；脚本子进程一律用**新终端口径**的 `PATH`
（机器级 + 用户级拼起来 = 1781 字符），否则 `capture` 与 `restore` 的分母会不同。
