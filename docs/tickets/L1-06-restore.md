## 目标

`tuoen restore <tuoen.d 目录>` —— 在新机器上重建整个 dev-state。**产出 plan → 展示 diff → 用户确认 → apply**，`--dry-run` 走同一套代码路径。

这是招牌功能的出口。它的验收物是**一份可信的 plan 和一次可复现的 apply**，而不是"跑完了"。

## 必须实现的

### 1. plan → diff → apply（统一引擎）

- 所有变更先产出 plan（可序列化纯数据）
- `--dry-run` 与真实执行走**同一套代码路径**（决策 20）
- 支持 `--only <section>` 做选择性还原（`tools` / `path` / `env` / `wsl`）—— 这是 `tuoen.d/` 分文件设计的直接收益
- 用户确认后才 apply

### 2. 按 section 的还原语义

| section | 做什么 | 不做什么 |
|---|---|---|
| `tools` | 用 L0 的安装引擎装缺失的工具 | **不接管**第三方版本管理器管的工具（见下） |
| `path` | 复用上一票的 diff + 重建 + 选择性应用 | 不原样复制（会把旧机器的重复一起移植） |
| `env` | 写缺失的环境变量（用户级默认；机器级产 `requires-elevation` 待办） | **不写任何密钥材料** |
| `wsl` | 报告 WSL 发行版差异 | 不自动导入 vhdx（体积与风险都太大） |

### 3. `manual_actions` —— **必须输出的人工待办清单**

`restore` 结束时**必须**输出这一段。条目类型固定（**这是公开契约**）：

| 类型 | 内容 | 硬性要求 |
|---|---|---|
| `requires-elevation` | 需要管理员的步骤（机器级环境变量、MSVC bootstrapper、Developer Mode） | 说明为什么需要，以及不提权时的降级路径 |
| `credential-reconfigure` | 需要手动重配的凭据 | **只写意图**（"`git:https://git.seawayos.com:8443` 这个凭据需要重配"），**绝不写材料** |
| `licence-blocked` | 因再分发许可被拒的制品 | **必须给出具体原因**，不是"许可问题"这种无用信息 |
| `third-party-manager` | 由第三方版本管理器管理的工具 | 说明"这里有 nvm4w 管理的 Node，需要你自己装" |
| `unsupported` | 本版本不支持的条目类型 | 说明原因 |

**`credential-reconfigure` 只写意图是安全红线**：本机 `C:\Users\Muelsyse\.m2\settings.xml` 里有明文 `glpat-` PAT —— 配置捕获会原样外泄。DPAPI / 凭据管理器 blob 是**用户+机器绑定**的，迁移后会**静默失败**（失败模式是困惑，不是被盗）。

### 4. 不接管第三方版本管理器

本机活证据：`NVM_HOME` / `NVM_SYMLINK` **同时存在于 HKCU 与 HKLM**（真实的双重管理腐坏）。接管 nvm4w 意味着改写它那个**需要管理员才能重建的符号链接**，且两个版本管理器会争抢同一个 reparse point。

→ **`restore` 只读识别，产出 `third-party-manager` 待办项，不碰它的符号链接 / 环境变量 / `settings.txt`。**

### 5. 幂等性

对已还原的机器再次 `restore` → plan 为空，输出"无变更"，退出码 0。

### 6. 部分失败时不留下半成品

- 每个 section 的 apply 必须原子（复用 L0 的原子安装原语）
- 中途失败 → 不留半个目录 / 半套环境变量
- 能从失败点继续（`--resume` 或幂等重跑）

### 7. 无网络时的行为

- 至少完成**元数据部分**（plan、`path` / `env` 的重建）
- **明确告诉我哪些步骤需要网络**，而不是笼统失败

## 明确不做

- 不自动提权
- 不迁移凭据（只输出意图）
- 不自动导入 WSL vhdx
- 不实现离线 bundle（L3）—— 但 plan 的**结构必须为它留位置**（`tuoen.lock` 里的上游 URL 与哈希就是接口）
- 不做 GUI

## 测试 seam 与硬性约束

**三层 seam 全用**，主 seam 是 **CLI 进程边界**（跑 `restore --dry-run`，断言 plan 的 JSON 结构）。

**硬性约束（违反即为 bug）**：

- **测试不得读写真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH` / 用户真实安装目录 / 真实 `%APPDATA%\tuoen`。**
- **测试不得访问真实网络。** 下载必须走可注入的传输层。
- **测试不得触碰 nvm4w 的符号链接 / 环境变量 / `settings.txt`。**
- **测试不得在开发机上真的执行一次完整 restore** —— 端到端测试全部在可注入的假根里做。

**必须有的用例**：

- **幂等**：`restore` 两次 → 第二次 plan 为空、输出"无变更"、退出码 0
- **`--dry-run` 与真实执行的 plan 逐条相同**
- `--only path` → 断言 `tools` / `env` 未被改动
- **`manual_actions` 五类各一个用例**，且 `credential-reconfigure` 的用例必须断言**输出里不含那个 token 的任何片段**
- `licence-blocked`：构造一个不可再分发的制品（如 Oracle JDK 8）→ 断言被拒**且原因具体**
- `third-party-manager`：fixture 里有 nvm4w 管理的 Node → 断言产出待办项，且**断言 nvm4w 的假注册表后端 / 假 `settings.txt` 未被写**
- **部分失败**：在某个 section 中途注入失败 → 断言不留半成品、可重入
- 无网络：传输层返回网络错误 → 断言元数据部分仍完成，且输出里指明了哪些步骤需要网络
- 空快照：`tuoen.d/` 为空 → 明确报错，不是静默成功

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- **在作者本机上真实跑一次 `tuoen restore --dry-run`**（用本机自己 `capture` 出的 `tuoen.d/` 作为输入 —— 这是最诚实的测试：还原到本机应当产出**接近空的 plan**，而不是一堆变更），把完整 plan 贴进 issue 评论
- **这个"接近空"的结果本身就是最重要的证据**：如果它产出了一堆变更，说明 `capture` 或 `restore` 有 bug（或本机真的有那么多问题 —— 那也要逐条解释）
- 贴出 `manual_actions` 的真实输出（预期至少含 `credential-reconfigure` 的 git 凭据意图，**且不含任何材料**）
- 贴出幂等性证据（连跑两次，第二次为空）

## 写入范围（advisory）

- `crates/core/`（restore plan / apply / `manual_actions`）
- `crates/cli/`（`restore` 子命令）
- `tests/`、`fixtures/`

**不要动**：`crates/manifest/`、`crates/shim/`
