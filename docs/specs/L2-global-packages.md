# spec: L2 扩展（全局包管理：npm -g / pip 的捕获、还原与按运行时版本隔离）

> 本 spec 是 `/to-spec` 的产物，来自 `docs/DESIGN.md` 的决策 26 / 27（全局包管理、全局包前缀）
> 与 L2 侦察期的**真机实测**（§Problem Statement 里每个数字都标了来源）。
> **术语以 `GLOSSARY.md` 为准**；与 `docs/DESIGN.md` 冲突时以 ADR 为准。
> 前序 spec：L0 `docs/specs/L0-install-engine.md`、L1 `docs/specs/L1-dev-state.md`。

---

## Problem Statement

L1 让 `tuoen capture` 能**看见**全局包（`globals.toml`，决策 165–188），但 `tuoen restore`
对它们**只能说"我不还原这一节"**（决策 188 让这句话被说出来，而不是静默忽略）。
于是今天的状态是：**清单能搬，包不能搬** —— 而"清单搬过去了、包没搬"正是最坏的一种成功：
新机器上 `globals.toml` 里写着 7 个包，用户以为自己有。

本机实测（L2 侦察期，2026-10-03）暴露了四个具体问题：

1. **全局包活在"会消失"的地方。** 本机 `npm config get prefix` = `C:\nvm4w\nodejs`，
   而它是 **`SymbolicLink` → `C:\Users\Muelsyse\AppData\Local\nvm\v24.19.0`**（实测 `Mode = l`）。
   `node_modules` 在**符号链接内部**（`…\nvm\v24.19.0\node_modules`，7 个包：`@deepseek-ai/dsh`、
   `codex`、`billion-context`、`corepack`、`npm`、`pnpm`、`tokentracker-cli`）。
   nvm4w 切一次 Node 版本，这 7 个包**一个都不会报错地消失** —— 没有工具会说"你少了 7 个全局包"。
   这正是决策 26 写的陷阱。

2. **npm 在 Windows 上给出的"命令"是三个文件，一个都不能用。** 实测 `C:\nvm4w\nodejs\pnpm`
   （403 B，POSIX shell）、`pnpm.cmd`（332 B）、`pnpm.ps1`（833 B）—— **没有 `.exe`**。
   而 `.cmd` / `.ps1` 在本仓库是**禁止发布**的（Node ≥18.20.2 之后无法被 spawn，CVE-2024-27980）。
   `.cmd` 的内容实测是"找 `%dp0%\node.exe`，找不到就用 PATH 上的 `node`，然后
   `node.exe "<prefix>\node_modules\pnpm\bin\pnpm.mjs" %*`" —— 也就是说，**它要干的事我们可以
   自己用 `.exe` 干，而且不用经过 `cmd.exe`**。另外实测一个包可以给出**多个** bin 名：
   `pnpm` 的 `package.json` 里 `bin = {"pnpm":"bin/pnpm.mjs","pnpx":"bin/pnpx.mjs","pn":"bin/pnpm.mjs","pnx":"bin/pnpx.mjs"}`。

3. **npm 的全局树没有 `.bin`，也没有 `.package-lock.json`**（实测：两者都不存在）。
   所以"自己走目录"既数不准（决策 167：只数到 5/7），也没有一份"谁是谁的 bin"的清单 ——
   唯一可靠的来源是每个包的 `package.json` 的 `bin` 字段，或者 npm 自己的回答。

4. **pip 那边的问题是相反的：它给的是能用的 `.exe`，但装在 PATH 之外。**
   实测真机 `<Python312>\Scripts\` 里是 `pip.exe` / `pip3.exe` / `pip3.12.exe` / `pypinyin.exe`
   （各 108 KB 的启动器），**它们是真正的 PE 可执行文件**。
   而 `python -m site --user-base` = `C:\Users\Muelsyse\AppData\Roaming\Python`（**本机这个目录
   不存在** —— 本机的 pip 包是装在 Python 安装目录里的全机位置）。
   pip 自己会把这件事说出来：往重定向后的 user base 装东西时它印
   `WARNING: The script pygmentize.exe is installed in '…' which is not on PATH.`

还有一条**每个 Windows 机器都要小心的**实测事实：本机 `where python` 的第一条是
`C:\Users\Muelsyse\AppData\Local\Microsoft\WindowsApps\python.exe` —— **App Execution Alias
（0 字节 reparse stub）**。执行它拿不到 Python，只会打开商店或什么都不做。
所以 L2 里**任何**要"问 Python"的地方都必须走**具体的可执行文件**（`pip.exe` / `…\python.exe`），
不许走 `python` 这个名字（这条是 L1 决策 25 的直接延续）。

---

## Solution

五个互相支撑的部分，全部复用 L0/L1 已有的引擎（`plan / diff / apply`、staging + junction 翻转、
shim 生成器、`FixtureProcess` 固定装置）：

### 1. `tuoen` 管理的全局包根（`globals root`）—— 按运行时版本隔离

新增一个**由 tuoen 拥有**的根：`%LOCALAPPDATA%\tuoen\globals\`。

- npm：`…\globals\npm\<node 版本字符串>\`，例如 `…\globals\npm\v24.19.0\`
  （重定向变量 `NPM_CONFIG_PREFIX`，**实测生效**：设成临时目录后 `npm config get prefix`
  在 302 ms 内改口，`npm install -g --offline` 真的装进那里，`npm ls -g --json --depth=0 --offline`
  真的只看得见那一个包）。
- pip：`…\globals\pip\`（**这一层不需要按版本分**：`PYTHONUSERBASE` 下面 pip 自己会插一层
  `Python312\`，实测 `user-base = <root>`、`user-site = <root>\Python312\site-packages`、
  console script 落在 `<root>\Python312\Scripts\`）。

**隔离是"按运行时版本"而不是"按工具"**：Node 换版本 → 换 prefix → 各版本的全局包互不干扰，
且**切版本不会让包消失**（它们不在被切掉的那个目录里）。这是决策 26 的直接实现。

**绝不写用户的配置文件**（决策 27）：不写 `.npmrc`、不写 `pip.ini`、不写 `pyvenv.cfg`。
重定向只通过**进程环境变量**：`NPM_CONFIG_PREFIX`、`PYTHONUSERBASE`、`PIP_USER=1`。
理由（决策 27 原文）：写 `.npmrc` 会覆盖用户自己的配置；pnpm 曾因仓库本地 `.npmrc` 展开 `${ENV}`
导致密钥外泄而不得不停止该行为。

### 2. 捕获：两个来源都要报，而且要说出它是哪个来源

`globals.toml` 的 `[[global]]` 增加一个**加性**字段 `source`（`"machine"` | `"tuoen"`，决策 166：
加法不递增 `schemaVersion`）：

- `source = "machine"` —— 今天的行为：问 `npm` / `pip` 它们自己的全局位置（决策 167/168/169）。
- `source = "tuoen"` —— 新增：问 tuoen 管理的那个根（npm 用 `--prefix`/环境变量指向它；
  pip 用重定向后的 `pip list --user --format=json`，**实测**这个调用在重定向下正好只列
  tuoen 管的那些，不重定向时是空的 `[]`）。

**为什么这条是必须的**：还原之后，新机器的 `npm ls -g` **不会**列出 tuoen 装的那些包
（它们在 tuoen 的 prefix 里，不在 `C:\nvm4w\nodejs` 里）。如果 `capture` 只问机器自己，
"刚还原完再捕获一次"会得到一份**少了所有 tuoen 管理的包**的清单 —— 一句看起来完全合理的假话，
而且是**迁移链条上最容易发生的一次**。两个来源都报，这类假话就没有藏身处。

### 3. 还原：`restore` 的 `globals` 节真的能装

`tuoen restore <dir> --apply` 的 `globals` 节：

1. **staging + 翻转**（复用 L0 的机制）：先装进 `…\globals\npm\<版本>\.staging-<n>\`，
   装完再翻成正式目录。**整节要么全成、要么一点都不变**（决策 156 的"每节原子"）。
2. **按快照里的精确版本装**（`pkg@<version>`），不是"满足约束的最新版"。
   快照里的版本就是那个版本；还原不是升级。
3. **`--offline` 走本地缓存**：实测 `npm install -g --offline pnpm@11.21.0` 在缓存命中时
   `added 1 package in 2s`（2170 ms）、退出码 0；缓存未命中时**快速失败**：
   `npm error code ENOTCACHED` + 退出码 1（458 ms）。所以"离线"的失败形态是明确的、快的，
   不是挂住。计划里的 `needsNetwork` 按这个算：**所有包都能在缓存里解决 → 不需要网络**。
4. **逐包结果**：一个包装不上（版本不存在、缓存没有、网络断）**不让整节崩**，
   该包记一条稳定的失败码（`install-failed` / `not-cached` / `version-not-found`），
   其余包继续；整节的 `outcome` 说清楚"7 个里成了 6 个"。
5. **诚实的不对称**：快照里的 `prefix`（本机是 `C:\nvm4w\nodejs`）**不是**我们装进去的地方。
   还原的人类输出必须**逐字说出来**："这 7 个包装在 `…\globals\npm\v24.19.0`，
   不在 `C:\nvm4w\nodejs`；`npm ls -g` 看不到它们，`tuoen globals list` 看得到。"
   计划里同时给一条 `manual_action`（`code = "globals-prefix-moved"`，只写意图）。
   **不说这句话的还原是假的成功**。
6. **绝不碰机器自己的 prefix**（决策 154）：不接管 nvm4w、不改 `C:\nvm4w\nodejs` 里的任何东西、
   不动 nvm4w 的 `settings.txt`。

### 4. 命令可用性：包 bin 的 `.exe` shim

装完的包要让用户**敲得出来**，否则"还原"只是把文件放对了地方。

- **npm 的包**：从 `<prefix>\node_modules\<包>\package.json` 的 `bin` 字段拿到 bin 名 → JS 路径，
  生成 tuoen 的 **`.exe` shim**（复用 L0 的 slot + 转发器机制），目标是
  `node.exe <prefix>\node_modules\<包>\<bin.js>`。
  **不生成 `.cmd` / `.ps1`**（本仓库禁止，且 `.cmd` 无法被 spawn）；
  **不解析 npm 生成的 `.cmd`**（它是 shell 脚本，不是契约；`bin` 字段才是）。
- **pip 的包**：console script 已经是 `.exe`（实测 `<root>\Python312\Scripts\pygmentize.exe`），
  所以 shim 直接转发到它。
- **shim 落在既有的 `%LOCALAPPDATA%\tuoen\shims`**（已经在 PATH 上）。
- **名字冲突必须报告，不许静默覆盖**：包的 bin 名与既有工具/包 shim 重名时，
  计划里出一条"这个 shim 会遮蔽 X"（沿用 L0-08 的遮蔽报告形状），由用户决定。

### 5. `tuoen globals list` —— 只读的管理视图

一张表：`工具 · 运行时版本 · 来源（machine/tuoen）· 包名 · 版本 · 提供的 bin 名`。
人类输出中文，`--json` 稳定不本地化。

**L2 不新增 `globals add` / `globals remove`。** 理由是本项目的定位句：
`tuoen` 是**开发环境管理器**，不是又一个包安装器（DESIGN §0）。
用户要往 tuoen 管理的根里加包，正确做法是 `tuoen shell` 之后正常 `npm i -g` / `pip install --user` ——
`tuoen shell` 会带上重定向变量（§Implementation Decisions 里"谁设置重定向"那条），
于是用户自己的命令**自动**落进 tuoen 的根，而 `capture` 能看见它（`source = "tuoen"`）。

---

## User Stories

### 换一台机器，把全局包也搬过去

> 我在旧机器上 `tuoen capture`，把 `tuoen.d/` 拷到新机器，`tuoen restore tuoen.d --apply`。
> 它告诉我：7 个 npm 包装进了 `…\globals\npm\v24.19.0`（不在 nvm4w 的 prefix 里）、
> 3 个 pip 包装进了 `…\globals\pip\Python312`，并且告诉我 `npm ls -g` 看不到它们、
> `tuoen globals list` 看得到。我在新机器上敲 `pnpm -v`，**能用**。

### 切 Node 版本，全局包不再消失

> 我用 nvm4w 从 v24 切到 v22。旧版本装的全局包还在 `…\globals\npm\v24.19.0` 里，
> 切回来就能用；新版本有一个自己的空 prefix，我不会看到"7 个包变成了 0 个"这种沉默。

### 我不想让它偷偷改我的东西

> `restore` 默认只出计划（决策 150）。计划里逐条写着"要装什么、装到哪、会创建哪些 shim、
> 哪一条会遮蔽现有的谁"。我没有 `.npmrc` 被改过，我的 `C:\nvm4w\nodejs` 一个字节没动，
> 我的 nvm4w `settings.txt` 哈希没变。

### 一个包装不上，我不想整节白跑

> 快照里 7 个包，第 5 个的版本在缓存和网上都找不到。它记了一条 `version-not-found`，
> 另外 6 个装好了，并且明确告诉我"7 个里成了 6 个"。

### 离线机器

> 目标机器没有网络。我 `tuoen restore --offline`，缓存里有的包装上了，
> 缓存里没有的**快速**失败并列出名字，而不是挂在那儿等 150 秒（issue #19）。

---

## Implementation Decisions

### 谁设置重定向变量（**本 spec 的新决定，决策 26/27 的具体化**）

**只有 tuoen 自己启动的子进程环境里才有这三个变量**，绝不写进 `HKCU\Environment`：

| 场景 | 行为 |
|---|---|
| `tuoen shell`（项目 pin 的 shell） | 设置 `NPM_CONFIG_PREFIX` / `PYTHONUSERBASE` / `PIP_USER=1` |
| `tuoen restore --apply` 的 `globals` 节 | 给 npm/pip 子进程设置（只在子进程环境里） |
| `tuoen capture` 的枚举 | 对 `source = "tuoen"` 的那一遍设置；对 `source = "machine"` 的那一遍**不设置** |
| 用户自己的终端 | **什么都不设** —— 用户的 `npm i -g` 照旧去机器自己的 prefix |

**为什么不写进注册表**：`NPM_CONFIG_PREFIX` 要按 Node 版本变（`…\npm\v24.19.0`），
而注册表里的一条静态值表达不了"当前 Node 版本"；写死一个共享 prefix 又违反决策 26 的隔离。
**为什么用户自己的终端不设**：那是"静默改变用户系统状态"，与决策 3 / 12 / 154 一脉相承 ——
用户的 `npm ls -g` 必须仍然说真话（说他自己装了什么），tuoen 管的那些由 `tuoen globals list` 说。
代价是**两套全局包并存**，所以 §2 的 `source` 字段和 §3.5 的"诚实的不对称"是这一票的必需部分，
不是可选项。

### `globals root` 的确切形状（冻结）

```
%LOCALAPPDATA%\tuoen\globals\
├─ npm\
│  └─ v24.19.0\                     ← node 版本字符串原样（带 v），就是 `node -v` 的输出
│     ├─ node_modules\<包>\…
│     ├─ pnpm / pnpm.cmd / pnpm.ps1 …（npm 自己生成，我们不删也不改）
│     └─ .staging-<n>\              ← 只在安装中出现；崩了也不留（L0 的规矩）
└─ pip\                             ← PYTHONUSERBASE 指向这一层
   └─ Python312\                    ← pip 自己插的版本层（实测）
      ├─ site-packages\…
      └─ Scripts\<命令>.exe
```

**版本目录名用 `node -v` 的原样字符串**（`v24.19.0`），不是我们归一化后的版本 ——
判据是"这是那个运行时自己的标识"，与决策 172 同源。**判不出来（`unknown`）时，这一节
不产生任何计划**：装到一个"不知道是哪个运行时"的目录里比不装更糟。

### 计划与报告的字段（冻结）

`restore --json` 的 `globals` 节在既有的 `sections[]` 里，逐包结果放在该节的
`actions[]`（沿用决策 164 的 `apply.sections[]` 形状）：

```jsonc
// plan 里（dry-run 与默认形态逐字节相同，决策 150）
{ "id": "globals", "status": "would-change", "needsNetwork": true,
  "actions": [
    { "kind": "install-global", "subject": "npm:pnpm@11.21.0",
      "detail": "→ %LOCALAPPDATA%\\tuoen\\globals\\npm\\v24.19.0" },
    { "kind": "create-shim", "subject": "pnpm" },
    { "kind": "shim-shadow", "subject": "pnpm", "detail": "遮蔽 C:\\nvm4w\\nodejs\\pnpm.cmd" }
  ],
  "manualActions": [ { "code": "globals-prefix-moved",
                       "subject": "npm",
                       "detail": "快照里的 prefix 是 C:\\nvm4w\\nodejs；装在 …\\globals\\npm\\v24.19.0" } ] }

// apply 之后（决策 164 的 apply.sections[] 里）
{ "id": "globals", "outcome": "applied", "wrote": true,
  "packages": [ { "tool": "npm", "name": "pnpm", "version": "11.21.0", "result": "installed" },
                { "tool": "pip", "name": "pypdf", "version": "6.19.0", "result": "version-not-found" } ],
  "shims": ["pnpm", "pnpx", "pn", "pnx"] }
```

`result` 的取值是稳定字符串：`installed` / `already-present` / `not-cached` /
`version-not-found` / `install-failed` / `skipped-shadowed`。

### 幂等（决策 155 的延续）

第二次 `restore` 在 tuoen 的根已经与快照一致时给 `no-change`，**不重装**。
判据是"枚举 tuoen 的根得到的 `(包, 版本)` 集合与快照里的 tuoen 集合相同"，**不是**"目录存在" ——
"目录存在"在装了 3 个包之后照样为真。

### 子进程输出里的凭据（L1 决策 178 的延续）

npm/pip 的输出可能带上 registry URL 里的 token（`//registry.npmjs.org/:_authToken=…`）。
所以：子进程的 stdout/stderr **只在报告里出现，且必须先过 L1 的 `secrets::scan_content`**；
**任何情况下不写进快照**。命中时把那段替换成 `[redacted:<shape>]` 并把这条记进报告。

### 超时（决策 170 的延续）

枚举用 `GLOBALS_TIMEOUT = 60s`；**安装用更长的 `GLOBAL_INSTALL_TIMEOUT = 300s`**
（实测一个包离线 2.2 s、联网 10.4 s，但冷缓存 + 慢镜像会显著更久）。
超时走逐包失败（`install-failed` + `timed-out`），不让整节挂住。

---

## Testing Decisions

### 硬性测试约束（**违反即为 bug**）

1. **测试里绝不出现网络**：`FixtureProcess` 记录 argv + 环境，断言我们**构造的命令行**；
   真实安装只在真机验收里做（且用 `--offline`）。
2. **测试绝不写真实的 `%LOCALAPPDATA%\tuoen\globals`**：用假文件系统 + `IsolatedHome`
   （`crates/cli/tests/common/mod.rs`），或写到 `%TEMP%` 下的临时根。
3. **测试绝不碰 `C:\nvm4w` / nvm4w 的任何状态**（决策 154）。
4. **每个"说不出来"的分支都要有反例用例**：`version-not-found`、`not-cached`、
   `install-failed`、`shim-shadow` 各至少一条。
5. **`--json` 的键名是契约**：`source` / `result` / `globals-prefix-moved` 三个新词都要有契约测试钉住
   （camelCase 字段、kebab-case 枚举、`Option` 一律 `skip_serializing_if`）。

### 必须有的具体用例

- **`NPM_CONFIG_PREFIX` 与 `PYTHONUSERBASE` 的构造**：断言子进程环境里恰好是
  `…\globals\npm\<node -v 原样>\` 与 `…\globals\pip\`，且 `PIP_USER=1`；
  并且断言**用户自己的环境（`HKCU\Environment`）一个字节都没被写过**。
- **`bin` 字段 → shim 名**：`{"pnpm":"bin/pnpm.mjs","pn":"bin/pnpm.mjs"}` 要生成两个 shim
  指向同一个 JS；`bin` 是字符串（不是对象）时按包名当一个命令（npm 的约定）。
- **`.cmd` 不参与**：断言产出的 shim 里**没有** `.cmd`/`.ps1`，且生成器**没有**读过
  `<prefix>\*.cmd`（固定装置里放一个内容不同的 `.cmd`，断言没人读它）。
- **staging + 翻转**：装到一半失败 → 正式目录**逐字节未变**、`.staging-` 不留残余。
- **幂等**：第二次计划 `no-change`；把 tuoen 的根里删掉一个包 → 计划变 `would-change` 且只装那一个。
- **两个来源**：`capture` 在"机器 7 个 + tuoen 3 个"的固定装置上产出 10 行，`source` 分别是
  `machine` / `tuoen`；再断言"只问机器"会漏掉那 3 个（这条是反例用例，防回归到假话）。
- **凭据**：子进程输出里塞一个 `glpat-` 形状 → 报告里是 `[redacted:…]`，快照里没有。
- **`python` 名字不许出现**：断言 pip 相关命令的 program 是 `pip.exe`（或 `…\python.exe -m pip`），
  **不是** `python`（本机 `where python` 第一条是 WindowsApp stub，实测）。

### 本 spec 不要求做的事

- 不要求支持 pnpm/yarn/conda 的全局包（决策 171 的工具表是 npm + pip；将来加不改格式）。
- 不要求"包之间的依赖关系"建模 —— 装的是包，依赖由 npm/pip 自己解决。
- 不要求并发安装（串行，顺序稳定，便于读日志）。
- 不要求 GUI（L3）。

---

## Out of Scope

- **不接管 nvm4w / 任何第三方版本管理器**（决策 154）。
- **不写用户的配置文件**（决策 27）。
- **不做 `globals add` / `globals remove`**（本 spec §Solution 5 的理由）。
- **不把 tuoen 的 globals 根加到 PATH** —— 命令通过 shim 暴露（L0 的机制），
  不改 PATH 是产品级约束（决策 3 / 12）。
- **不还原机器级（`HKLM`）的 Python/npm 安装** —— 那是 `tools` 节的事。
- **不保证 npm 生成的 `.cmd`/`.ps1` 可用** —— 我们生成自己的 `.exe` shim；
  npm 自己写的那些文件我们不删、不改、不依赖。

---

## Further Notes

### 为什么 L2 比 L1 更容易写出"看起来对"的错代码

L1 的错误形态是"说错了一句话"（把正常的报成坏的）。L2 的错误形态更贵：
**它会真的往磁盘上装东西**，而且装错的地方**不会立刻报错** ——
装进机器自己的 prefix 里，用户当下觉得"成功了"，直到切 Node 版本的那一天。
所以这一票里"装到哪"必须比"装了什么"更早被钉住：
计划里逐条写出目标路径，`manualActions` 里写出不对称，`tuoen globals list` 里能看见来源。

### 本机取证的四个"必须变成测试"的实例

1. `NPM_CONFIG_PREFIX` 生效（302 ms 改口）→ 契约测试钉住环境变量的名字与形状。
2. 离线装成功 `added 1 package in 2s` / 离线失败 `ENOTCACHED` 458 ms → 钉住两种失败形态。
3. `PYTHONUSERBASE` 下的 `<root>\Python312\Scripts\*.exe` → 钉住 shim 的目标路径形状。
4. `pnpm` 的 `bin` 字段一个包四个名字 → 钉住"shim 数 ≠ 包数"。

### 与 `mise` 的关系

`mise` 不做全局包管理（它管工具版本），所以这一票没有直接参照物；
但它的 `bootstrap` 资源抽象与本 spec §2 的"两个来源"是同一种问题：
**同一个问题有几个答案时，把每个答案的来源说出来，而不是挑一个当唯一真相。**
