## 目标

补齐 dev-state 的两个缺失 section，让捕获面真正完整：**全局包清单（`globals.toml`）** 与 **配置文件清单（`configs.toml`）**。

这一票不是"顺手加两个文件"——它包含 L1 里**第二个安全红线**（配置文件扫描会直接碰到本机那个明文 PAT）。

## 必须实现的

### 1. `globals.toml` —— 全局包清单，**按工具版本隔离**

```toml
schema_version = 1

[[global]]
tool = "npm"                    # npm | pnpm | yarn | pip | ...
tool_version = "24.19.0"        # 该清单属于哪个 Node/Python 版本
prefix = "C:\\nvm4w\\nodejs"    # 全局包前缀
prefix_inside_version_dir = true
packages = [
  { name = "@deepseek-ai/dsh", version = "0.1.0-rc.6" },
  ...
]
```

**`tool_version` 是必需的**，理由是本机的活陷阱：`npm config get prefix` = `C:\nvm4w\nodejs` —— **全局 npm 包装进了符号链接内部**，切 Node 版本会**静默隐藏**它们。只做捕获还原则清单会与实际状态脱节而用户不会察觉。

`prefix_inside_version_dir` 必须由代码判定（前缀是否落在版本目录或 reparse point 内部），不能靠用户填。

**本机预期数据**：npm 全局包 `@deepseek-ai/dsh@0.1.0-rc.6`、`@openai/codex@0.160.0`、`billion-context@0.1.179`、`corepack@0.35.0`、`npm@11.17.0`、`pnpm@11.21.0`、`tokentracker-cli@0.87.3`。

### 2. `configs.toml` —— 配置文件清单（含"跳过原因"）

```toml
schema_version = 1

[[config]]
path = "C:\\Users\\Muelsyse\\.gitconfig"
kind = "git"
captured = true
content_hash = "sha256:..."
# 或者是：
[[config]]
path = "C:\\Users\\Muelsyse\\.m2\\settings.xml"
kind = "maven"
captured = false
skip_reason = "contains-credential-shape"    # 必须具体
```

**必须捕获的**（本机实测的清单）：`~/.gitconfig`、`~/.npmrc`、`~/.m2/settings.xml`、`~/.docker/config.json`、`~/.ssh/config`（若存在）、`~/.wslconfig`、VS Code `settings.json`、JetBrains 配置目录标记。

**关键洞见（必须体现在实现里）**：本机 `~/.gitconfig` **几乎为空**（只有一行 `[credential "..."] provider = generic`），而**全局 `user.name` / `user.email` 均未设置**，系统配置提供 `core.autocrlf` / `credential.helper=manager` / `http.sslbackend=schannel` / `init.defaultbranch=master`。
→ **只快照 `~/.gitconfig` 会只拿到 3 行并丢掉整个 Git 身份。** 实现必须**同时读系统级与全局级 git 配置**并在输出里标注来源层级。

### 3. 配置文件扫描的安全红线

**原则不变：只捕获意图，绝不捕获材料。**

- 扫描配置文件内容，命中疑似凭据形状的 → **`captured = false` + 具体 `skip_reason`**
- **`skip_reason` 必须具体**（`contains-credential-shape` / `unreadable` / `too-large` / `binary`），不能是"跳过"
- **静默跳过是 bug**

**本机实证的判据**：`C:\Users\Muelsyse\.m2\settings.xml` 里有**明文 GitLab PAT**（`<server><id>gitlab-maven</id>`，用户 `zhangpengzhan`，`glpat-` 开头）。
同时 `~/.docker/config.json` 是 `credsStore: "desktop"` + 空 `auths` 块 → **安全**；`~/.ssh` 只有 `known_hosts`(99 B) → **无密钥**；JetBrains 目录有 `c.kdbx` / `c.pwd` / `idea.key` → **跳过**。

**"吓人的地方是安全的，安全的地方是危险的"** —— 环境变量里零密钥，而配置文件里有。实现不得假设"某个路径安全"。

### 4. 不捕获的东西（必须在跳过清单里可见）

- 缓存（本机约 8 GB：pnpm store 4,741.8 MB、`.cache\codex-runtimes` 1,614.5 MB、npm-cache 967.1 MB、`.m2\repository` 461.6 MB、pip Cache 347.3 MB）
- 任何凭据材料
- 项目代码

## 明确不做

- **不安装全局包**（那是 L2）—— 本票只捕获与报告
- 不做凭据迁移
- 不捕获 `~/.m2/repository`（缓存，不是状态）
- 不实现远程索引

## 测试 seam 与硬性约束

主 seam：**CLI 进程边界**（跑 `capture --only globals` / `--only configs`，读磁盘断言）。

**硬性约束（违反即为 bug）**：

- **测试不得读取真实的 `HKCU\Environment` / 用户真实配置文件 / 真实 `%APPDATA%`。**
- 测试不得执行真实的 `npm ls -g` / `pip list`（**不得依赖开发机已装的东西**，也不得访问网络）—— 命令执行必须走可注入的执行器。
- fixture 里**必须**放一个形似 `glpat-` 的 token 文件，用来证明排除真的生效。

**必须有的用例**：

- **密钥排除：fixture 里的 `glpat-` token 不出现在任何输出文件里，且跳过清单里有它、`skip_reason` 具体**
- `prefix_inside_version_dir` 判定：前缀指向版本目录 → `true`；指向普通目录 → `false`
- `tool_version` 关联：同一个 npm 前缀在两个不同 Node 版本下 → **两份独立清单**
- 全局包命令**失败**（未安装 npm）→ 报告"发现但无法枚举"，**不报错退出**
- git 配置层级：系统级 + 全局级都被读到，且标注来源；模拟本机情况（全局只有 credential 行、`user.name` 在系统级或缺失）→ 断言**不会丢掉 Git 身份信息**（缺失就明确说缺失）
- 缓存目录进入跳过清单
- `configs.toml` 的 `content_hash` 幂等（同一文件两次捕获 → 同哈希）
- 不可读文件 → `skip_reason = "unreadable"`，不崩溃

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- **在作者本机上真实跑一次** `tuoen capture --only globals` 与 `--only configs`，把两份产出**以及跳过清单**贴进 issue 评论
- **确认 `.m2/settings.xml` 被跳过且原因具体**（这是本票最重要的证据）
- 确认 `.m2/repository` 等缓存目录出现在跳过清单里
- 把 `globals.toml` 的 npm 清单与取证报告对照（预期 7 个包）

## 写入范围（advisory）

- `crates/core/`（全局包探测、配置文件清单）
- `crates/platform/`（命令执行器抽象、配置文件读取）
- `crates/cli/`（`capture` 的 `--only globals|configs`）
- `tests/`、`fixtures/`

**不要动**：`crates/manifest/`、`crates/shim/`
