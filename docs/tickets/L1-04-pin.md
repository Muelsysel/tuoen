## 目标

把项目级工具链 pin 做成可用功能，**且绝不引入"陌生仓库自动改我工具链版本"这个安全漏洞**。

这一票与 `detect` 只有一条边（`tuoen shell` 要用检测引擎判断某个 pin 的版本是否已安装），因此可以与 `capture` / `doctor` 并行推进。

## 必须实现的

### 1. `tuoen.toml` —— 项目级 pin 声明（进 git）

单文件，声明**一整条互相约束的工具链**（JDK + Node + Python 版本相互约束，分开声明会漂移）：

```toml
[project]
name = "my-project"

[tools]
node = "24"
python = "3.12"
java = "17"
```

版本可以是精确版本或前缀（`"24"` 匹配 24.x）。

### 2. `tuoen.lock` —— 解析后的精确版本（进 git）

由 `tuoen lock`（或 `tuoen shell` 首次解析时）产出。**必须记录上游来源与哈希**，为 L3 的离线 bundle 留接口。

`tuoen.toml` 与 `tuoen.lock` **不一致时必须被告知**，而不是静默按其中一个执行（story 48）。

### 3. `tuoen shell` —— **永远可用，不需要信任**

- 启动一个子 shell，把 pin 的版本目录**前置**进该子进程的 `PATH`
- **不翻转全局 junction**（本 spec 的新决定）—— 翻转是全局副作用，多终端场景下会让别的终端突然换版本
- **不影响父进程**，退出后环境回到原样
- 子 shell 必须是**真实的交互式 shell**（cmd 或 PowerShell），不是假装

**已知限制必须显式报告**：机器级 `PATH` 条目（如 `C:\nvm4w\nodejs`）可能遮蔽我们前置的目录。启动时做一次遮蔽检测，命中就在 **stderr 打印明确警告**（含被遮蔽的条目名），**不静默继续**。这与决策 12（绝不静默改写系统状态）一致。

### 4. `tuoen trust` —— 自动切换的信任门

- `tuoen trust`（当前目录）：记录**绝对路径 + 首次信任时的指纹**
- `tuoen trust --list`
- `tuoen trust --revoke <path>`
- 信任记录在 **`%APPDATA%\tuoen\trust.toml`**（用户级中央清单）—— **绝不放在被信任的目录内**，否则任何 clone 下来就自带信任标记，等于没有机制

**指纹的定义（必须精确实现）**：

> 指纹 = `tuoen.toml` 的**规范化内容 SHA-256**（先按 `\n` 归一化行尾、去掉每行行尾空白）。

- **不用**目录 mtime、inode 或 `tuoen.toml` 之外的文件 —— 前两者不稳定，后者会让任何源码改动都触发重新信任
- **行尾 CRLF ↔ LF 差异必须通过**（归一化后相同）
- 内容改一个字符 → 指纹不匹配 → **拒绝自动切换并提示重新 trust**（不是静默信任，也不是永久拉黑）

### 5. `tuoen auto` —— 自动切换（默认关闭）

进入一个**已信任且指纹匹配**的目录时自动应用 pin。
未信任的目录 → **不自动执行**，只在必要时提示"这个目录有 `tuoen.toml`，跑 `tuoen trust` 可启用自动切换"。

### 6. `trust.toml` 格式预留企业策略覆盖

```toml
schema_version = 1

[[trusted]]
path = "C:\\Work\\my-project"
fingerprint = "sha256:..."
trusted_at = "2026-10-02T12:00:00Z"

[policy]
source = "user"             # user | system
```

系统级策略路径**只预留格式，不实现**：`%PROGRAMDATA%\tuoen\policy.toml`。

### 7. 未安装版本的清晰提示

pin 一个**没有安装的版本**时给出清晰提示，并说明下一条该跑什么命令（story 47）。

## 明确不做

- **不实现企业策略的实际执行** —— 只预留格式
- 不实现 `tuoen use`（全局 junction 翻转）—— 那是 L0 的范围
- 不做 shell 钩子（PowerShell profile 注入 / `cd` 拦截）—— 自动切换在 L1 由用户显式调用 `tuoen auto` 触发；**自动挂进 shell profile 是独立议题**（它要改用户配置，需要自己的 plan/diff/apply 与警告）
- 不探测全局包

## 测试 seam 与硬性约束

主 seam：**CLI 进程边界**。

**硬性约束（违反即为 bug）**：

- **测试不得读取真实的 `HKCU\Environment` / 真实 `PATH` / 用户的真实 `%APPDATA%\tuoen`。** 信任清单路径必须可注入。
- **测试不得翻转真实的 junction。**
- **测试不得触碰 nvm4w 的符号链接 / 环境变量 / `settings.txt`。**
- 测试不得访问真实网络。
- **不得依赖真实交互式 shell** —— 子 shell 的构造逻辑必须可测（用一个假的"shell 启动器"注入，断言被传了什么环境与 `PATH`），端到端测试用非交互模式（如 `tuoen shell --exec "<cmd>"`）。

**必须有的用例**：

- **`tuoen shell` 不翻转全局 junction** —— 断言 junction 目标在 `shell` 前后**未变**
- `tuoen shell` 子进程 `PATH` **前置**了 pin 的版本目录
- 遮蔽检测：构造一个机器级条目遮蔽前置目录 → **stderr 出现警告**，且警告里含被遮蔽的条目名
- 指纹：内容相同 → 通过；改一个字符 → 拒绝；**CRLF ↔ LF 差异 → 通过**
- 未信任目录 → **不自动切换**（断言环境未变）
- `--revoke` 后 → 不再自动切换
- `tuoen.toml` 与 `tuoen.lock` 不一致 → 明确告知
- pin 未安装的版本 → 提示含下一步命令
- 信任清单写入**可注入路径**，不碰真实 `%APPDATA%`

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- **在作者本机上真实演示一次**：在一个临时目录里写 `tuoen.toml`、`tuoen shell --exec "node -v"`、贴出输出，并**证明全局 junction 未被改动**（前后各贴一次 junction 目标）
- 贴出"未信任目录不自动切换"的证据
- 贴出 `tuoen trust --list` 的输出

## 写入范围（advisory）

- `crates/core/`（pin 解析、lock 文件、信任清单、指纹）
- `crates/cli/`（`shell` / `trust` / `auto` / `lock` 子命令）
- `tests/`、`fixtures/`

**不要动**：`crates/manifest/`、`crates/shim/`
