## 目标

`tuoen doctor` 对机器状态做体检，**只报告，不修改**。每条发现带稳定 ID、严重度、来源与置信度。

**这一票的价值在于它是产品可信度的第一道门**：把"`PATH` 上确认可执行"与"注册表声称已装但文件缺失"并列展示，会让用户看一眼就不再信任这个工具。

## 输出契约

```
id            稳定机器可读 ID（如 path.duplicate）—— 未来要能按 ID 忽略/聚焦
severity      error | warn | info
message       中文人类可读（默认中文优先）
evidence      具体是哪几条（路径、变量名、条目索引）
source        这条结论从哪来（registry / path-resolution / filesystem / …）
confidence    与检测引擎同源的置信度
```

`--json` 输出必须稳定、**不本地化**（决策 35）。**每条发现的 ID 是公开契约**，改动即为破坏性变更。

## 检查项清单（全部要实现）

### `path.*`

| ID | 严重度 | 判据 | 本机预期 |
|---|---|---|---|
| `path.duplicate` | warn | 重复条目（忽略大小写） | 10 组（忽略大小写 12 组） |
| `path.missing` | warn | 指向不存在路径 | 7 条 / 5 个不同目标 |
| `path.username-hardcoded` | **error** | 含 `C:\Users\<名>\` | 11 条，**2 条在机器级** |
| `path.shadowed` | **error** | 我们的 shim 被机器级条目遮蔽 | `C:\nvm4w\nodejs`、Oracle `java8path` |
| `path.length-budget` | **error**（超限）/ info | 距 8191 悬崖的余量 | 1781，余量充足 |
| `path.empty-entry` | warn | 空条目 | HKLM `Path` 里有 `;;` |
| `path.reparse` | info | 条目是 reparse point | 2 个（junction + symlink） |
| `path.relative` | **error** | 相对路径条目 | 0（**相对路径永远受 MAX_PATH 限制**，`\\?\` 无法加前缀） |
| `path.non-ascii` | warn | 非 ASCII 条目 | 0（换机可能引入） |
| `path.spaces` | info | 含空格条目 | 14/48 = 29% |

**严重度分配原则**：会导致**静默失效**的是 `error`；会导致**突然全体失效**的是 `error`；只是噪声的是 `info`。

### `env.*`

| ID | 严重度 | 判据 | 本机预期 |
|---|---|---|---|
| `env.missing-target` | warn | 变量指向不存在的目标 | `HALCONROOT` |
| `env.duplicated-scope` | **error** | 同名变量同时在用户级与机器级 | `NVM_HOME` / `NVM_SYMLINK` |
| `env.name-with-spaces` | info | 变量名含空格 | `IntelliJ IDEA` |
| `env.path-literal` | warn | 值里字面含 `%VAR%` 但类型不是 `REG_EXPAND_SZ`（或反之） | 0 —— 这是 `setx` 事故的典型残留 |

`env.duplicated-scope` 为什么是 `error`：本机 `NVM_HOME` / `NVM_SYMLINK` **同时存在于 `HKCU\Environment` 和 `HKLM\Session Manager\Environment`** —— 这是**真实的双重管理腐坏症状**，不是理论问题。

### `tool.*`

| ID | 严重度 | 判据 | 本机预期 |
|---|---|---|---|
| `tool.multi-manager` | **error** | 同一逻辑工具被多个机制管理 | Node 同时被 nvm4w 管（且我们管时） |
| `tool.multiple-active` | **error** | 同一工具在 `PATH` 上解析到多个不同安装 | **Java：`java` → 1.8.0_491（Oracle JRE）、`javac` → 1.8.0_492（另一厂商）** |
| `tool.ghost` | warn | 幽灵条目 | `Python311`（9 个卸载键、2 条 PATH、`Test-Path` False） |
| `tool.global-prefix-inside-version-dir` | **error** | 全局包前缀落在版本目录 / reparse point 内部 | `npm config get prefix` = `C:\nvm4w\nodejs`（**符号链接内部**） |
| `tool.unmanaged-directory` | info | 开发相关目录从未被任何机制提到 | `C:\Dev\Tool\apache-maven-3.9.5`（1,408.8 MB，但 `~/.m2/repository` 461.6 MB 证明在用） |

`tool.global-prefix-inside-version-dir` 为什么是 `error`：切 Node 版本会**静默隐藏**全局安装的包，用户不会察觉。

### `system.*`

| ID | 严重度 | 判据 |
|---|---|---|
| `system.elevated` | info | 当前是否提权 |
| `system.developer-mode` | info | Developer Mode 状态（决定 symlink 是否可用）—— **值名必须自己确认，不要照抄未经验证的注册表路径** |
| `system.long-paths` | info | `LongPathsEnabled` |
| `system.wsl-nonstandard-path` | info | WSL 发行版 vhdx 不在默认位置（本机 `C:\linux\Arch-Linux-current`） |

## 明确不做

- **绝不修改任何东西。** 不写注册表、不写环境变量、不删条目、不"修复"。破坏性动作留给用户显式触发（决策 24）。
- 不提供 `--fix`
- 不探测全局包内容（只看前缀位置）—— 后续票

## 测试 seam 与硬性约束

主 seam：**CLI 进程边界** —— 跑 `doctor --json`，断言 finding 的 ID / 严重度 / 数量。

**硬性约束（违反即为 bug）**：

- **测试不得读取真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH`。**
- **诊断检查项的测试必须用固定装置（fixture），不用本机实时状态** —— 否则测试结果会随开发机漂移，而本机 `PATH` 恰好是坏的（这会让"测试通过"变得毫无意义）。
- fixture 素材**应当来自本机取证的真实形状**（本机那些坏 `PATH` 就是最好的 fixture 素材）。
- 测试不得访问真实网络。

**必须有的用例**（每个检查项至少一个正例 + 一个反例）：

- `path.duplicate`：忽略大小写的重复必须被抓到
- `path.username-hardcoded`：机器级与用户级各一条
- `path.length-budget`：**边界值 1719 / 8190 / 8191 / 8192** —— 8191 与 8192 的行为差异必须被明确断言
- `path.empty-entry`：`;;` 与开头/结尾的空条目
- `tool.multiple-active`：两个不同安装提供同名工具的不同子命令（模拟 `java` / `javac` 分裂）
- `tool.ghost`：注册表有键、文件不存在 → 报告，且**不产生任何删除动作**（断言磁盘与注册表后端未被写）
- `env.duplicated-scope`：同名变量两个 scope
- `tool.global-prefix-inside-version-dir`：前缀指向一个 reparse point
- **每个正例都必须有对应的反例**（健康的 fixture 不应产生该 finding）—— 否则检查项会变成永远报错的噪声源

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- **在作者本机上真实跑一次 `tuoen doctor`**，把**完整输出**贴进 issue 评论，并逐条与取证报告对照（预期能命中：`path.duplicate` 10 组、`path.missing` 7 条、`path.username-hardcoded` 11 条其中 2 条机器级、`env.duplicated-scope` 的 NVM_*、`tool.multiple-active` 的 Java 分裂、`tool.ghost` 的 Python311、`tool.global-prefix-inside-version-dir` 的 npm prefix）
- **任何与取证报告不符的地方都要在评论里说明**（预期不符就是 bug 或取证有误，两者都值得记录）
- 断言无写操作

## 写入范围（advisory）

- `crates/core/`（诊断检查项、finding 模型）
- `crates/cli/`（`doctor` 子命令）
- `tests/`、`fixtures/`

**不要动**：`crates/manifest/`、`crates/shim/`
