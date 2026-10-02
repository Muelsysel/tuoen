## 目标

`tuoen capture` 把检测结果 + 机器状态写成 **`tuoen.d/` 目录（分文件）**。这一票的验收物是**磁盘上的真实文件**，不是终端输出。

## 分文件结构

```
tuoen.d/
  ├─ tools.toml     工具与版本（含来源与置信度）
  ├─ path.toml      PATH 结构（所有者、顺序、长度预算）
  ├─ env.toml       环境变量（区分用户级 / 机器级）
  ├─ wsl.toml       WSL 发行版与实际 vhdx 路径
  └─ schema.toml    格式版本
```

**分文件不是为了整齐，而是为了选择性操作**："PATH 变了"与"Java 版本变了"是两个独立 diff，`tuoen restore --only path` 因此在**格式层面**天然成立，不需要事后打补丁。

后续票会往这个目录里加 `configs.toml` 与 `globals.toml` —— 本票要保证**加文件不需要改已有文件的结构**。

## 必须实现的

### 1. 每个文件带 `schema_version` 与 `captured_at`

`schema_version = 1`。`captured_at` 必须能被显式忽略（见幂等性要求）。

### 2. `tools.toml`

来自上一票的检测引擎，每条带 `name` / `version` / `path` / `source` / `confidence` / `manager` / `evidence`。
**不允许出现没有来源的条目。**

### 3. `path.toml` —— 这是这一票最重的部分

必须捕获**结构**而不只是一串字符串：

```toml
schema_version = 1

[budget]
raw_user_len = 725          # 字符数，未展开
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

- `scope` 必须区分 **`process-only`** —— 本机实测有 77 字符的进程注入条目（PowerShell MSIX 别名）**不在任何注册表里**。只读注册表的实现会漏掉它们，而它们真的在 `PATH` 上。
- `has_username` 必须检测硬编码的 `C:\Users\<名>\`（本机 11 条，**2 条在机器级**）
- `reparse` 必须区分 **junction / symlink / app-exec-alias**（本机 `PATH` 上有 2 个：Oracle 的 Junction、nvm4w 的 SymbolicLink）
- `raw` 与 `expanded` 都要存 —— **读取时用 `DoNotExpandEnvironmentNames` 拿原始值**，再单独展开

### 4. `env.toml`

区分用户级（`HKCU\Environment`）与机器级（`HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`）。
每个变量记录：`name` / `value_raw` / `value_expanded` / `scope` / `reg_type`（`REG_SZ` vs `REG_EXPAND_SZ`）/ `target_exists`。

本机有 13 个用户级变量，其中 `IntelliJ IDEA` 的**变量名含空格**（合法，但很多工具会静默跳过）—— 必须能正确往返。

### 5. `wsl.toml`

WSL 发行版名、状态、`BasePath`、vhdx 大小。**必须记录实际路径**：本机 `Arch-Linux-current` 的 `BasePath` 是 `C:\linux\Arch-Linux-current` —— **手工导入的非标准位置**，只 glob 默认路径的工具会完全漏掉它。

### 6. 密钥扫描：主动扫描 + 排除 + **报告跳过清单**

**这是本票的安全红线。原则：只捕获意图，绝不捕获材料。**

- 捕获时扫描环境变量与（后续票里的）配置文件，命中疑似凭据形状的**不写入 `tuoen.d/`**
- 输出里必须有一份**跳过清单**，说明**跳过了什么、为什么**
- **静默跳过是 bug** —— 它会让用户以为"都备份好了"

**本机实测的判据依据**：环境变量里**零密钥**（对 68 个进程变量做正则扫描只命中假阳性），而 `C:\Users\Muelsyse\.m2\settings.xml` 里有**明文 GitLab PAT**（`glpat-` 开头）—— **吓人的地方是安全的，安全的地方是危险的**。本票扫描环境变量；配置文件扫描是后续票，但**跳过清单的格式现在就定下来**。

### 7. `--only <section>` 与 `--out <dir>`

- `--only` 可重复，只捕获指定 section
- 默认写到当前目录的 `tuoen.d/`，`--out` 可改

### 8. 幂等性（必须测）

同一状态下连续两次 `capture`，除 `captured_at` 外 `tuoen.d/` 内容**逐字节相同**。

## 明确不做

- 不诊断（不产出 finding）—— 下一票
- 不探测全局包（`globals.toml`）—— 后续票
- 不扫描配置文件（`configs.toml`）—— 后续票
- **不写入除 `tuoen.d/` 之外的任何位置**
- **不触碰 nvm4w 的符号链接 / 环境变量 / `settings.txt`**

## 测试 seam 与硬性约束

主 seam 仍是 **CLI 进程边界**：跑 `capture`，然后**读磁盘上的文件**断言。

**硬性约束（违反即为 bug）**：

- **测试不得读取真实的 `HKCU\Environment` / `HKLM` / 真实 `PATH` / 用户真实安装目录。** 全部走可注入的根与假后端。
- **测试不得访问真实网络。**
- fixture 必须是**固定装置**，不依赖开发机实时状态。
- **`captured_at` 必须可从比较中排除** —— 否则幂等性测试会永远失败。

**必须有的用例**：

- 幂等：捕获两次 → 除 `captured_at` 外逐字节相同
- **密钥排除：fixture 里放一个形似 `glpat-` 的 token，断言它不出现在任何输出文件里，且跳过清单里有它的条目**
- `process-only` 条目：一个只在进程 `PATH` 里、不在注册表里的条目必须被捕获
- `scope` 正确性：同一个变量同时在用户级与机器级（本机 `NVM_HOME` / `NVM_SYMLINK` 就是这样）→ **两条都捕获，且 scope 不同**
- `has_username`：机器级与用户级各一条
- `reparse`：junction 与 symlink 各一条 fixture
- `env` 变量名含空格：往返不丢
- `--only path` → 只产出 `path.toml`（与 `schema.toml`），不产出其他
- `--out` 到临时目录 → 不污染工作目录

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- **在作者本机上真实跑一次 `tuoen capture`**，并把产出的 `tuoen.d/path.toml` 的 `[budget]` 段与**重复条目计数**贴进 issue 评论，与取证报告对照（预期：`raw_user_len` 725、`raw_machine_len` 978、`effective_len` 1781、重复 10 组）
- **确认产出的文件里不含任何密钥材料**（贴出跳过清单作为证据）

## 写入范围（advisory）

- `crates/core/`（捕获逻辑、`tuoen.d/` 序列化）
- `crates/platform/`（注册表读、reparse point 判定、WSL 读取）
- `crates/cli/`（`capture` 子命令）
- `tests/`、`fixtures/`

**不要动**：`crates/manifest/`、`crates/shim/`
