# spec: L1 可用（整机 dev-state 捕获与还原 + 项目级 pin + 环境体检诊断）

> 本 spec 是 `/to-spec` 的产物，来自七轮设计拷问（`docs/DESIGN.md`，35 条决策）与六条 ADR（`docs/adr/`）。
> **术语以 `GLOSSARY.md` 为准**；与 `docs/DESIGN.md` 冲突时以 ADR 为准。
> L0（安装引擎）已完成前序 spec：`docs/specs/L0-install-engine.md`。

---

## Problem Statement

一台 Windows 开发机上，"我的开发环境是什么"这个问题的答案**不存在于任何单一位置**。
本机取证显示它散落在至少八个互不重叠的来源里：

1. `PATH` 解析结果（48 条，含 10 组重复、7 条失效、1 个拼错 3 次的目录）
2. `App Paths` 注册表（HKLM 42 + HKCU 19 = 61 条）——**纯 PATH 扫描会整个漏掉这套查找机制**
3. 文件系统上的 reparse point（2 个在 PATH 上：Oracle 的 Junction、nvm4w 的 SymbolicLink）
4. App Execution Aliases（`WindowsApps\{python.exe,…}`，reparse tag `0x8000001b`、长度 0）——**既不是文件也不是符号链接**，`Test-Path` 会通过但复制什么也拿不到
5. 三个 hive 的卸载键（HKCU 11 / HKLM 78 / WOW6432Node 30）
6. MSIX 包状态
7. `winget list` 的状态（约 157 行，混了四套命名空间，且**幽灵 `Python 3.11` 有 9 个活卸载键、winget 照样报告已安装**）
8. **任何注册表都没提到的目录**——`C:\Dev\Tool\apache-maven-3.9.5`（1,408.8 MB）既不在 winget 也不在 PATH，但 `~/.m2/repository`（461.6 MB）证明它在用；同样不可见的还有 tomcat / minio / nacos / redis

后果是具体且已发生的：本机 `java` 解析到 1.8.0_491（Oracle JRE，经 Junction），
`javac` 解析到 1.8.0_492（另一个厂商的 JDK8）——**同一条工具链的两个命令来自两个不同安装**。
换一台机器时，这些都不会自己被发现，而"原样搬运"会把上面那 10 组重复和 7 条死条目一起移植过去。

同时，`PATH` 本身是一个**没有被任何工具当作资源建模**的东西：没有所有者、没有顺序语义、
没有长度预算，而它有两个不同的悬崖（`setx` 在 1024 字符处**静默裁剪**；
`cmd.exe` 在 **8191 字符**后**完全忽略整条 `Path`**）。

最后，项目级工具链版本声明（`tuoen.toml`）如果"进入目录就自动生效"，
则一个 clone 下来的陌生仓库可以 pin 一个带后门的 Node 版本——**这是安全漏洞，不是便利性权衡**。

---

## Solution

三个互相支撑的能力，共用 L0 已经建好的 `plan / diff / apply` 引擎：

### 1. 捕获（capture）——把 dev-state 读成机器可读形式

`tuoen capture` 产出 `tuoen.d/` 目录（分文件），每个文件对应一类状态：

```
tuoen.d/
  ├─ tools.toml     工具与版本（含来源与置信度）
  ├─ path.toml      PATH 结构（所有者、顺序、长度预算）
  ├─ env.toml       环境变量
  ├─ configs.toml   配置文件清单（含"跳过原因"）
  ├─ wsl.toml       WSL 发行版与路径
  └─ globals.toml   全局包清单（按工具版本隔离）
```

**分文件不是为了整齐，而是为了选择性操作**："PATH 变了"与"Java 版本变了"是两个独立 diff，
`tuoen restore --only path` 因此在**格式层面**天然成立，不需要事后打补丁。

**逐项标注来源与置信度**（决策 21 / 25）：每一条工具记录都带 `source`（`tuoen` /
`nvm4w` / `winget` / `registry-arp` / `app-paths` / `filesystem-scan` / …）与 `confidence`
（见下方"检测信任层级"）。**不允许出现没有来源的条目。**

**只捕获意图，绝不捕获材料**（决策 23）：捕获的每一条都必须是版本 / 路径 / 名字；
任何 token、密文、DPAPI blob、密钥文件都**不得**进入 `tuoen.d/`。
主动扫描 + 排除 + **报告跳过清单**——被跳过的东西必须出现在输出里并说明原因，
因为静默跳过会让用户以为"都备份好了"。

### 2. 体检诊断（doctor）——在搬运之前先知道哪里是坏的

`tuoen doctor` 对**已捕获的状态**（或实时状态）做检查，每条发现都带类别、严重度、来源与置信度。
检查项见下方 Implementation Decisions。

诊断**只报告，不修改**。"自动清理注册表和 PATH"出错即无法挽回，而检测本身也可能误判
（例如把网络驱动器上的真实安装当幽灵条目）。破坏性动作留给用户显式触发。

### 3. 还原（restore）——在新机器上重建

`tuoen restore <tuoen.d 目录>` 产出 plan → 展示 diff → 用户确认 → apply。
`--dry-run` 走**同一套代码路径**（决策 20）。支持 `--only <section>` 做选择性还原。
还原结束时输出一份**需要人工完成的事项清单**（带外管理员前置条件、需要手动重配的凭据、
因许可被拒的制品），而不是假装全自动。

### 4. 项目级 pin

`tuoen.toml`（进 git）声明整条工具链的版本约束；`tuoen.lock` 记录解析后的精确版本。
两种生效方式，**共用同一个解析结果**：

- `tuoen shell` —— **永远可用，不需要信任**。启动一个子 shell，把 pin 的版本目录
  **前置**进该子进程的 `PATH`，**不翻转全局 junction**（因此不影响其他终端）。
- `tuoen auto`（自动切换）—— 需要 `tuoen trust` 过一次。**默认不自动执行。**

---

## User Stories

### 捕获

1. 作为开发者，我想运行一条命令就把整机开发状态落成文件，这样我能把它提交进一个仓库。
2. 作为开发者，我想看到每个被捕获的工具**是从哪里发现的**（我们装的 / nvm4w 装的 / winget 装的 / 只有目录），这样我知道哪些能在新机器上自动重建。
3. 作为开发者，我想让捕获结果**明确区分**"确认可执行"与"注册表声称已装但文件不存在"，这样我不会把幽灵条目当成真实安装。
4. 作为开发者，我想让捕获结果**告诉我它跳过了什么以及为什么**（密钥、缓存、无法读取的配置），这样我不会误以为备份是完整的。
5. 作为开发者，我想捕获 `PATH` 的**结构**（每个条目的所有者、顺序、是否含变量、是否含用户名、是否指向 reparse point），而不只是一串字符串。
6. 作为开发者，我想捕获环境变量时区分**用户级与机器级**，这样我知道哪些还原时需要提权。
7. 作为开发者，我想捕获的 `PATH` 条目带**长度预算**信息，这样我在新机器上不会撞上 8191 悬崖。
8. 作为开发者，我想捕获 `WSL` 发行版时记录它们的**实际 vhdx 路径**，这样手工导入到非标准位置的发行版（如本机 `C:\linux\Arch-Linux-current`）不会漏掉。
9. 作为开发者，我想捕获**全局包清单**（`npm -g` / `pip`），并且**按工具版本隔离**，这样切换 Node 版本后我知道哪些包"消失"了。
10. 作为开发者，我想捕获结果**不包含任何密钥材料**，这样我可以安全地把它提交进公开仓库。
11. 作为开发者，我想捕获**已存在的第三方安装**（只读采纳），这样在作者这种机器上（Node 是 nvm4w 装的、Python 是官方装的、JDK 是手工解压的）这个功能不是空的。
12. 作为开发者，我想让捕获是**幂等**的：同一台机器上跑两次，第二次的 diff 为空（除时间戳等显式排除的字段）。
13. 作为开发者，我想能只捕获一个 section（如 `--only path`），这样我能快速看某个方面的现状。
14. 作为开发者，我想捕获结果带一个**schema 版本号**，这样未来的格式变更能被检测而不是静默错读。

### 体检诊断

15. 作为开发者，我想知道 `PATH` 上有多少**重复条目**，分别是哪几条。
16. 作为开发者，我想知道 `PATH` 上有多少**失效条目**（指向不存在的路径），分别是哪几条。
17. 作为开发者，我想知道 `PATH` 上有多少条目**硬编码了用户名**，其中几条在机器级——因为换账号名后它们会静默失效，而 `Test-Path` 在原机器上仍然通过。
18. 作为开发者，我想知道哪些 `PATH` 条目**遮蔽**了我们（机器级条目排在用户级之前，让我们的 shim 永远轮不到），以及是哪些条目。
19. 作为开发者，我想知道 `PATH` 的**当前长度**与距离 8191 悬崖还有多少余量，在超限时得到明确警告而不是"所有命令突然都找不到"。
20. 作为开发者，我想知道哪些环境变量**指向不存在的目标**（如本机 `HALCONROOT`）。
21. 作为开发者，我想知道哪些环境变量**同时存在于用户级和机器级**——这是双重管理腐坏的活症状（本机 `NVM_HOME` / `NVM_SYMLINK`）。
22. 作为开发者，我想知道同一个逻辑工具是否被**多个机制同时管理**（如 Node 同时被 nvm4w 和我们管），以及哪个会赢。
23. 作为开发者，我想知道 `PATH` 上的条目**哪些是 reparse point**，以及是 junction 还是 symlink——因为 symlink 在未提权的新机器上可能无法重建。
24. 作为开发者，我想知道**全局包前缀是否落在版本目录内部**（本机 `npm config get prefix` = `C:\nvm4w\nodejs`，即符号链接内部），因为切版本会静默隐藏它们。
25. 作为开发者，我想知道系统里有多少**幽灵条目**，但**不要**工具自动清理它们。
26. 作为开发者，我想知道机器上有哪些**开发相关目录从未被任何注册表或 PATH 提到**（如本机 Maven），因为这是"换电脑时最容易丢"的一类。
27. 作为开发者，我想知道当前的**提权状态**与 **Developer Mode 状态**，因为它们决定哪些还原步骤可行。
28. 作为开发者，我想让诊断结果能输出成 `--json`，这样我能把它接进 CI 或未来的 GUI。
29. 作为开发者，我想让诊断的**每条发现都有稳定的机器可读 ID**（如 `path.duplicate`），这样我能按 ID 忽略或聚焦某类问题。

### 还原

30. 作为开发者，我想在还原前看到**完整的 plan**：会装什么、会写哪些环境变量、`PATH` 会变成什么样。
31. 作为开发者，我想看到 `PATH` 还原的**逐条 diff**（新增 / 删除 / 保持 / 修复），并能**选择性应用**——因为原样搬运会把旧机器的 10 组重复一起移植。
32. 作为开发者，我想让 `--dry-run` 与真实执行走同一套代码路径，这样预览不会与执行漂移。
33. 作为开发者，我想让还原**幂等**：重复执行不产生额外变更，输出"无变更"。
34. 作为开发者，我想让还原**部分失败时不留下半成品**（原子性），并能从失败点继续。
35. 作为开发者，我想在还原结束时得到一份**人工待办清单**：需要管理员权限的步骤、需要手动重配的凭据（只说"这里有一个 git 凭据需要重配"，不含材料）、因许可被拒的制品及具体原因。
36. 作为开发者，我想让还原**拒绝**因许可不可再分发的制品，并给出**具体原因**（不是"许可问题"这种无用信息）。
37. 作为开发者，我想让还原**不去动第三方版本管理器的状态**（nvm4w 的符号链接、环境变量、settings.txt），只在输出里说明"这里有 nvm4w 管理的 Node，需要你自己装"。
38. 作为开发者，我想让还原能在**没有网络**时至少完成元数据部分并明确告诉我哪些步骤需要网络。

### 项目级 pin

39. 作为开发者，我想用一个 `tuoen.toml` 声明整个项目的工具链版本，因为它们互相约束。
40. 作为开发者，我想让 `tuoen.lock` 记录解析后的精确版本，这样同事和我拿到完全一样的工具链。
41. 作为开发者，我想让 `tuoen shell` **不需要任何信任标记就能用**，因为它是我显式执行的命令。
42. 作为开发者，我想让 `tuoen shell` 里的版本切换**只影响这个子 shell**，这样我不用为了跑一个项目而改掉整台机器的默认版本。
43. 作为开发者，我想让自动切换**默认关闭**，并且只对**我显式信任过的目录**生效。
44. 作为开发者，我想让 `tuoen trust` 记录**绝对路径 + 首次信任时的指纹**，这样别人删掉目录再 clone 一个同名目录不会继承我的信任。
45. 作为开发者，我想能列出并**撤销**已信任的目录。
46. 作为开发者，我想让信任清单**可被系统级策略覆盖**（企业场景），格式上现在就预留。
47. 作为开发者，我想在 `tuoen.toml` 里 pin 一个**没有安装的版本**时得到清晰的提示，并知道下一条该跑什么命令。
48. 作为开发者，我想在 `tuoen.toml` 与 `tuoen.lock` 不一致时被告知，而不是静默按其中一个执行。

---

## Implementation Decisions

### 检测信任层级（决策 25 的具体化）

每条检测到的工具记录带一个 `confidence`，取值与判据：

| `confidence` | 判据 | 本机实例 |
|---|---|---|
| `managed` | 由 `tuoen` 自己安装（存在我们的安装记录） | （L0 完成后才有） |
| `executable` | 在 `PATH` 上能解析到，且目标**不是** App Execution Alias，且文件真实存在、大小 > 0 | `javac` → `C:\Dev\base\JDK\JDK8\bin\javac.exe` |
| `registered-missing` | 注册表声称已安装，但文件不存在 → **幽灵条目** | `Python311` |
| `directory-only` | 发现目录，未在任何注册表 / `PATH` / `App Paths` 里注册 | `C:\Dev\Tool\apache-maven-3.9.5` |
| `alias-ghost` | 是 App Execution Alias（reparse tag `0x8000001b`、长度 0），`Test-Path` 通过但**不是文件** | `WindowsApps\python.exe` |
| `manager-owned` | 由第三方版本管理器管理（只读采纳） | nvm4w 的 Node |

**`alias-ghost` 是必须独立存在的一类**：本机 `Get-Command python` **成功**并返回一个 0 字节的
App Execution Alias，排在 `PATH` 最前——如果不显式识别 reparse tag，它会被误报成"Python 可用"。

### 诊断检查项清单（`tuoen doctor`）

每条检查有一个稳定 ID、一个严重度（`error` / `warn` / `info`）、以及"来源"字段。

**`path.*`**
- `path.duplicate` —— 重复条目（本机 10 组，忽略大小写 12 组）
- `path.missing` —— 指向不存在路径的条目（本机 7 条 / 5 个不同目标）
- `path.username-hardcoded` —— 硬编码 `C:\Users\<名>\`（本机 11 条，**2 条在机器级**）
- `path.shadowed` —— 我们的 shim 被机器级条目遮蔽（本机：`C:\nvm4w\nodejs`、Oracle `java8path`）
- `path.length-budget` —— 距 8191 悬崖的余量
- `path.empty-entry` —— 空条目（本机 HKLM `Path` 里有 `;;`）
- `path.reparse` —— 条目是 reparse point，标注 junction / symlink
- `path.relative` —— 相对路径条目（**相对路径永远受 MAX_PATH 限制**，`\\?\` 无法加前缀）
- `path.non-ascii` —— 非 ASCII 条目（本机没有，但换机可能引入；shim 与脚本会踩坑）
- `path.spaces` —— 含空格条目（本机 14/48 = 29%），作为信息项提示"依赖正确引号化的地方"

**`env.*`**
- `env.missing-target` —— 变量指向不存在的目标（本机 `HALCONROOT`）
- `env.duplicated-scope` —— 同名变量同时在用户级与机器级（本机 `NVM_HOME` / `NVM_SYMLINK`）
- `env.name-with-spaces` —— 变量名含空格（本机 `IntelliJ IDEA`）——合法的，但很多工具会静默跳过
- `env.path-literal` —— 变量值里**字面**含有 `%VAR%` 但类型不是 `REG_EXPAND_SZ`（或反之），这是 setx 事故的典型残留

**`tool.*`**
- `tool.multi-manager` —— 同一逻辑工具被多个机制管理
- `tool.multiple-active` —— 同一工具在 `PATH` 上解析到多个不同安装（本机 Java：`java` 与 `javac` 来自两个厂商）
- `tool.ghost` —— 幽灵条目（**只报告**）
- `tool.global-prefix-inside-version-dir` —— 全局包前缀落在版本目录 / reparse point 内部（本机 npm prefix = `C:\nvm4w\nodejs`）
- `tool.unmanaged-directory` —— 开发相关目录从未被任何机制提到

**`system.*`**
- `system.elevated` —— 当前是否提权
- `system.developer-mode` —— Developer Mode 状态（决定 symlink 是否可用）
- `system.long-paths` —— `LongPathsEnabled`
- `system.wsl-nonstandard-path` —— WSL 发行版 vhdx 不在默认位置

**严重度分配原则**：会导致**静默失效**的是 `error`（`path.username-hardcoded`、`env.duplicated-scope`、
`tool.global-prefix-inside-version-dir`）；会导致**突然全体失效**的是 `error`（`path.length-budget` 超限）；
只是噪声的是 `info`（`path.spaces`）。

### `tuoen.d/` 的文件 schema 骨架

所有文件带 `schema_version = 1` 与一个 `captured_at`（**`--json` 与幂等比较时必须忽略它**）。

```toml
# tuoen.d/tools.toml
schema_version = 1
captured_at = "2026-10-02T12:00:00Z"

[[tool]]
name = "node"
version = "24.19.0"
source = "nvm4w"
confidence = "manager-owned"
path = "C:\\Users\\Muelsyse\\AppData\\Local\\nvm\\v24.19.0"
manager = "nvm4w"
```

```toml
# tuoen.d/path.toml
schema_version = 1

[budget]
raw_user_len = 725          # 字符数（不含展开）
raw_machine_len = 978
effective_len = 1781
cliff = 8191

[[entry]]
scope = "machine"           # user | machine | process-only
owner = "third-party"       # tuoen | third-party | system | unknown
raw = "C:\\Program Files\\OpenSSH\\"
expanded = "C:\\Program Files\\OpenSSH\\"
exists = true
reparse = "none"            # none | junction | symlink | app-exec-alias
has_vars = false
has_username = false
dup_index = 0               # 同值条目里的序号，0 = 首次出现
```

### `tuoen shell` 与 junction 的关系（本 spec 的新决定）

**`tuoen shell` 不翻转全局 junction。** 它把 pin 的版本目录**前置**进子进程的 `PATH`。
理由：翻转 junction 是**全局副作用**，在多终端场景下会让别的终端突然换版本；
而项目级 pin 的语义是"在这个项目里用这个版本"。
`tuoen use <tool>@<version>` 才是**显式**的全局翻转（L0 已定义）。

**已知限制必须显式报告**：机器级 `PATH` 条目（如 `C:\nvm4w\nodejs`）可能遮蔽我们前置的目录。
`tuoen shell` 启动时做一次遮蔽检测，命中就在 stderr 打印明确警告（含被遮蔽的条目名），
**不静默继续**。这与决策 12（绝不静默改写系统状态）一致。

### 指纹的定义（决策 16 的具体化）

`tuoen trust` 记录的指纹用于检测"目录被替换"。指纹 = `tuoen.toml` 的
**规范化内容 SHA-256**（不含行尾差异：先按 `\n` 归一化并去掉行尾空白）。
**不**用目录 mtime、inode 或 `tuoen.toml` 之外的文件——前两者不稳定，后者会让任何源码改动都触发重新信任。
指纹不匹配时的行为：**拒绝自动切换**并提示重新 `trust`（不是静默信任，也不是永久拉黑）。

### 信任清单格式（决策 17 的具体化）

`%APPDATA%\tuoen\trust.toml`：

```toml
schema_version = 1

[[trusted]]
path = "C:\\Work\\my-project"
fingerprint = "sha256:..."
trusted_at = "2026-10-02T12:00:00Z"

# 企业策略覆盖预留：若存在 HKLM 级策略文件，它优先于本文件
[policy]
source = "user"             # user | system
```

系统级策略路径（**本 spec 只预留格式，不实现**）：`%PROGRAMDATA%\tuoen\policy.toml`。

### 还原的人工待办清单（决策 12 / 23 / 32 的汇总出口）

`tuoen restore` 结束时**必须**输出一个 `manual_actions` 段，条目类型固定为：

- `requires-elevation` —— 需要管理员的步骤（机器级环境变量、MSVC bootstrapper、Developer Mode）
- `credential-reconfigure` —— 需要手动重配的凭据，**只写意图**（"`git:https://git.seawayos.com:8443` 这个凭据需要重配"），绝不写材料
- `licence-blocked` —— 因再分发许可被拒的制品 + **具体原因**
- `third-party-manager` —— 由第三方版本管理器管理的工具，需要用户自行安装
- `unsupported` —— 本版本不支持的条目类型 + 原因

### 幂等性的定义

捕获的幂等：同一状态下连续两次 `capture`，除 `captured_at` 外 `tuoen.d/` 内容逐字节相同。
还原的幂等：对已还原的机器再次 `restore`，plan 为空，输出"无变更"且退出码为 0。
**两者都要有测试**——幂等是最容易悄悄坏掉的属性。

---

## Testing Decisions

**沿用 L0 的三层 seam，不新增第四层**（见 `docs/specs/L0-install-engine.md`）：

1. **CLI 进程边界（主 seam）** —— 跑真实二进制，断言 stdout / stderr / 退出码 / 磁盘副作用。
   L1 的绝大多数验收都在这一层：`capture` → 检查 `tuoen.d/` 内容；`doctor` → 检查 `--json` 结构；
   `restore --dry-run` → 检查 plan。
2. **`core` 的 plan 生成 seam** —— plan 是可序列化纯数据，不起进程就能测 diff 算法。
   `PATH` 重建的 diff 逻辑必须在这一层有密集的单测（它有最多分支）。
3. **`platform` 适配器 seam** —— 仅测平台差异时用。

### 硬性测试约束（**违反即为 bug**）

- **测试不得读取或写入真实的 `HKCU\Environment`、真实 `PATH`、用户真实安装目录。**
  所有路径与注册表访问必须经过可注入的根与后端。L1 比 L0 更危险，因为它**读**的东西更多，
  而"读一下真实 PATH"看起来无害、直到某个测试开始写它。
- **测试不得访问真实网络。**
- **测试不得触碰 nvm4w 的符号链接、环境变量或 `settings.txt`。**
- **诊断检查项的测试用固定装置（fixture），不用本机实时状态**——
  否则测试结果会随开发机漂移，而本机的 `PATH` 恰好是坏的（这会让"测试通过"变得毫无意义）。

### 必须有的具体用例

- `PATH` 重建 diff：新增 / 删除 / 保持 / 修复 / 顺序变化 / 仅大小写差异，各一个用例
- 检测信任层级：六种 `confidence` 各一个 fixture 用例，**`alias-ghost` 必须有**（reparse tag `0x8000001b` + 长度 0）
- 幽灵条目：注册表有键、文件不存在 → 报告为 `registered-missing`，且**不产生任何删除动作**
- 用户名依赖：机器级与用户级各一条
- 8191 预算：1719 / 8190 / 8191 / 8192 边界值
- 密钥排除：fixture 里放一个形似 `glpat-` 的 token，断言**它不出现在任何输出里**，且**跳过清单里有它的条目**
- 信任指纹：内容相同 → 通过；内容改一个字符 → 拒绝自动切换；行尾 CRLF↔LF 差异 → **通过**（归一化）
- 幂等：捕获两次、还原两次
- 部分失败：还原中途失败 → 断言不留半成品、可重入
- `tuoen shell`：断言**全局 junction 未被改动**，且子进程 `PATH` 前置了 pin 的目录

### 本 spec 不要求做的事

- 不要求跨平台测试（V1 只发 Windows）。
- 不要求性能基准（`doctor` 与 `capture` 是交互式命令，不是关键路径；**shim 是**，但它在 L0）。
- 不要求对真实 `winget` / `scoop` 的集成测试（只读检测用 fixture 模拟它们的输出）。

---

## Out of Scope

- **全局包管理（`npm -g` / `pip` 的安装与卸载）** —— L2。L1 只**捕获清单**（`globals.toml`），不安装。
- **离线 bundle** —— L3。L1 的 `restore` 需要网络来下载。
- **GUI** —— L3。
- **自动修复** —— `doctor` **只报告**。任何"自动清理 PATH / 注册表"都不在本 spec 内。
- **企业策略的实际执行** —— 只预留 `trust.toml` 的 `[policy]` 段与格式，不实现策略强制。
- **代码签名** —— 独立议题（见 `docs/adr/0006`）。
- **接管第三方安装** —— 明确不做（见 `docs/adr/0004`）。
- **跨平台** —— V1 只发 Windows 二进制。
- **凭据的迁移** —— 明确不做，只输出"需要重配"的意图（见 `docs/adr/0004` 与决策 23）。

---

## Further Notes

### 为什么 L1 比 L0 更容易写出"看起来对"的错代码

L0 的错误会当场暴露：解压失败、校验不过、junction 建不出来。
L1 的错误**全部是静默的**——多报一个重复条目、漏掉一条用户名依赖、
把幽灵条目算成可用、指纹比较忘了归一化行尾。
所以本 spec 对 L1 的测试要求比 L0 更严：**每个检查项都要有 fixture 用例，且 fixture 必须来自本机取证的真实形状**（本机那些坏 PATH 就是最好的 fixture 素材）。

### 本机取证的三个"必须变成测试"的实例

1. **`WindowsApps\python.exe`** —— 0 字节、reparse tag `0x8000001b`、`Get-Command` 成功。
   任何"`Test-Path` 通过就算存在"的实现都会在这里给出错误答案。
2. **`NVM_HOME` / `NVM_SYMLINK` 同时在 HKCU 与 HKLM** —— 这是真实的双重管理腐坏。
   `env.duplicated-scope` 检查项的存在理由就是它。
3. **`npm config get prefix` = `C:\nvm4w\nodejs`**（一个符号链接内部）——
   切 Node 版本会静默隐藏全局包。这是 `tool.global-prefix-inside-version-dir` 的存在理由，
   也是 L2（全局包管理）必须解决的坑，L1 只负责**发现并报告**。

### 与 `mise` 的关系

`mise` 有 `bootstrap` / dotfiles / services / secrets / repos，但它的系统资源（`accounts` /
`macos defaults` / `launchd` / `systemd`）是 **Linux/macOS 形状**，**没有 Windows 注册表 / 可选功能 /
系统设置资源**，且 Windows 上只支持 shim。**L1 的差异化正在这里**：把 Windows 特有的
`PATH` 结构、reparse point、注册表卸载键、App Execution Alias、WSL vhdx 位置
当作**一等资源**来建模。实现中遇到具体问题时第一参考对象仍是 `mise` 的源码与 issue 区。
