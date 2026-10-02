## 目标

实现 **检测引擎（detection engine）**：把"这台机器上装了什么开发工具"读成结构化记录，**每条都带来源与置信度**。这是 L1 四个能力（捕获 / 诊断 / 还原 / pin）共用的地基，所以这一票单独切出来。

## 必须实现的

### 1. `tuoen detect --json` 命令（内部能力，但必须有 CLI 出口以便端到端验收）

输出 `Vec<DetectedTool>`，字段固定：

```
name          逻辑工具名（node / python / java / git / ...）—— **小写，是"工具"不是"命令"**
version       探测到的版本（可能为空 —— 空表示"发现但无法确定版本"）
path          主路径（可执行文件或安装根目录）
source        tuoen | path-resolution | app-paths | registry-arp | filesystem-scan | manager
confidence    见下
manager       第三方版本管理器名（如 nvm4w），无则为空
evidence      人类可读的一句话，说明这条是怎么被发现的
reproducible  这一条能不能被 tuoen 在新机器上自动重建（由 confidence 派生）
```

**关于 `name` 是"工具"不是"命令"**（实现时定的，理由来自真机输出）：
`java` 与 `javac` 是**同一个工具的两个命令**，不是两个工具。一次真机运行让这条变得必需 ——
`node` 本来会报 4 行（`node.exe` / `npm.cmd` / `npx.cmd` / `corepack.cmd`，版本分别是
`24.19.0` / `11.17.0` / `11.17.0` / `0.35.0`），四个不同的版本号挤在同一个工具名下，
用户没法回答"我的 Node 是哪个版本"。所以**只报主命令**（`node` / `python` / `java` / `git` / …），
次要命令仍然被发现（shim 需要知道 `npm.cmd` 在哪），只是不重复占用报告的行。

**`source` 的取值与 L1 spec 里列的略有不同**：去掉了 `nvm4w` 与 `winget` 两个取值 ——
前者被 `manager` + `manager` 字段取代（否则每加一个版本管理器就要加一个枚举取值，
而"哪个管理器"已经有专门的字段了），后者属于"只读检测第三方安装器"的后续票。

### 2. 七个 `confidence` 层级，判据必须精确

| 值 | 判据 |
|---|---|
| `managed` | 存在 tuoen 自己的安装记录（L0 完成后才有） |
| `executable` | 在 `PATH` 上能解析到，目标**不是** App Execution Alias，文件真实存在且**大小 > 0** |
| `registered` | **注册表声称已装且安装目录真的存在，但它不在 `PATH` 上** |
| `manager-owned` | 由第三方版本管理器管理（只读采纳） |
| `directory-only` | 发现目录，未在任何注册表 / `PATH` / `App Paths` 里注册 |
| `registered-missing` | 注册表声称已安装，但文件不存在 → 幽灵条目 |
| `alias-ghost` | 是 App Execution Alias（**reparse tag `0x8000001b`、长度 0**），`Test-Path` 通过但**不是文件** |

**`alias-ghost` 必须独立存在。** 本机实测：`WindowsApps\python.exe` 是 0 字节、tag `0x8000001b`，而 `Get-Command python` **成功**并排在 `PATH` 最前。任何"`Test-Path` 通过就算存在"的实现都会在这里给出错误答案。

**`registered` 是独立验收逼出来的第七层**（原始票据只有六层）。ARP 的 `InstallLocation`
常常是一个**目录**：本机 `C:\Program Files\Git\`、`C:\Program Files\Java\jre1.8.0_491\`、
`C:\Program Files\WSL Dashboard\`。把它们报成 `executable` 违反了上面那一行自己的判据
（"在 `PATH` 上能解析到、文件真实存在且大小 > 0" —— 这三条一条都不满足）。
它与 `directory-only` 的区别是**有注册表记录**（因此可重建），
与 `registered-missing` 的区别是**目录真的在**（所以不是幽灵）。

### 3. 检测来源（至少这些）

- **PATH 解析** —— 按进程 `PATH` 顺序解析每个可执行文件名，记录**解析到哪个**以及**是否被更早的条目遮蔽**
- **App Paths 注册表** —— `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths` + HKCU 对应位置。本机实测 42 + 19 = 61 条，**纯 PATH 扫描会整个漏掉这套查找机制**
- **三个 hive 的卸载键（ARP）** —— HKCU / HKLM / WOW6432Node，读 `DisplayName` / `DisplayVersion` / `InstallLocation` / `UninstallString`
- **文件系统扫描** —— 已知开发根目录（`C:\Dev`、`C:\Tools`、`C:\Software`、用户目录下的常见位置等）里**看起来像工具安装**的目录
- **第三方版本管理器识别** —— nvm4w（环境变量 `NVM_HOME` / `NVM_SYMLINK` + `settings.txt`）、uv、pyenv-win、asdf/mise 的 shim 目录
- **版本探测** —— 对已知工具跑 `<tool> --version` 类命令。**注意：`git --version` 与 `java -version` 输出到 stderr，`node -v` 到 stdout → 必须同时读两路**。必须设超时（一个挂住的工具不能挂住整个 detect）

### 4. 只读

**这一票只读，绝不写任何东西。** 不写注册表、不写环境变量、不建链接、不删文件。

## 明确不做

- 不捕获（不写 `tuoen.d/`）—— 那是下一票
- 不诊断（不产出 finding）—— 那是再下一票
- 不探测全局包（npm -g / pip）—— 后续票
- 不扫描配置文件 —— 后续票
- **不触碰 nvm4w 的符号链接 / 环境变量 / `settings.txt`**（只读环境变量与 `settings.txt` 是允许的）

## 测试 seam 与硬性约束

**沿用 L0 的三层 seam**（见 `docs/specs/L0-install-engine.md`），主 seam 是 **CLI 进程边界**。

**硬性约束（违反即为 bug）**：

- **测试不得读取真实的 `HKCU\Environment` / `HKLM` 注册表 / 真实 `PATH` / 用户真实安装目录。** 全部走可注入的根与假后端。L1 比 L0 更危险，因为它**读**的东西更多，而"读一下真实 PATH"看起来无害、直到某个测试开始写它。
- **测试不得访问真实网络。**
- **测试不得依赖开发机的实时状态** —— fixture 必须是**固定装置**。本机 `PATH` 恰好是坏的，用实时状态会让"测试通过"毫无意义。

**这份约束被拆成了两半，因为"真实现的接线对不对"用假后端测不出来**（实现时定的）：

- `crates/cli/tests/detect_contract.rs` —— **契约测试，严格遵守上面三条**。
  只断言形状与不变量（`--json` 信封、七层六源的键集合、取值不本地化、
  `summary` 计数自洽、两次运行逐字节一致、`--help` 列出全部层级）。
  **它不出现"这台机器上一定有 node"这类断言** —— 在干净 CI 上必须同样通过。
- `crates/cli/tests/real_machine_acceptance.rs` —— **真机验收，有意读这台机器**。
  它覆盖三件假后端**原理上测不到**的事：
  ① `RealRegistry` 拼的注册表路径对不对（机器级环境变量在
  `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment`，
  **不在 `SOFTWARE` 下面**，拼错了只会静默读到空）；
  ② `RealFileSystem` 从真实 NTFS 读到的 reparse tag 对不对
  （`alias-ghost` 整层建立在 `0x8000001b` 上，假文件系统的 tag 是我们自己喂的）；
  ③ 真的不会去执行那个 0 字节的别名（假运行器不会启动应用商店）。

  这一份用文件名与文件头注释把偏离写在明处，而不是混进契约测试里让约束**看起来**被遵守了。

**必须有的用例**：

- 七个 `confidence` 各一个 fixture 用例，**`alias-ghost` 必须有**（构造 tag `0x8000001b` + 长度 0 的 fixture）
- 幽灵条目：注册表有键、文件不存在 → `registered-missing`，且**不产生任何删除动作**，且**不报版本**（注册表的 `DisplayVersion` 是安装器的内部版本号，它看起来像真版本号，所以比"未知"更糟）
- 版本探测：stdout 与 stderr 各一个用例（模拟 `node -v` 与 `java -version` 的差异）
- 版本探测超时：模拟一个不返回的工具 → 记录"发现但版本未知"，**不挂住**
- App Paths：一条只在 App Paths 里、不在 `PATH` 里的条目能被发现
- 遮蔽：机器级条目与用户级同名可执行文件 → 记录解析到哪个、以及被谁遮蔽
- `managed` 层：有安装记录时能报出来（L0 未落地时这一层恒为空，但判据必须在）

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- `cargo run -p tuoen-cli -- detect --json` 在**本机**能跑出真实结果，且**人工核对至少 3 条**：`java` 与 `javac` 解析到**两个不同厂商**的安装；`WindowsApps\python.exe` 被标为 `alias-ghost`；nvm4w 的 Node 被标为 `manager-owned`
- 上述人工核对的结果**贴进本 issue 的评论**（这是这一票的"证据"，不是可选步骤）
- 没有任何写操作（可用文件系统监视或代码审查证明）

**关于"`java` 与 `javac` 解析到两个不同厂商"这条**：真机结果证明了**分裂是真的**，
但它的形状与票据原文设想的不同 —— 不是"两条记录、厂商不同"，而是：

```
java    1.8.0_492  executable  C:\Dev\base\JDK\JDK8\bin\java.exe        ← Amazon（javac 也在这里）
java    1.8.0_491  executable  C:\Program Files (x86)\Common Files\Oracle\Java\java8path\java.exe  ← Oracle，一个 Junction
```

即：**`javac` 只有 JDK8 那一份（1.8.0_492），而 `java` 在 `PATH` 上被一个 Oracle 的
Junction 抢走了（1.8.0_491）**。用户敲 `java -version` 与 `javac -version` 会得到
两个不同厂商的版本 —— 这正是要抓的分裂，只是它表现为"两条 `java` 记录"而不是
"一条 `java` + 一条 `javac`"。

## 写入范围（advisory）

- `crates/core/`（检测引擎逻辑）
- `crates/platform/`（注册表 / reparse point / 版本探测的平台原语）
- `crates/cli/`（`detect` 子命令）
- `tests/`（fixture 与端到端用例）
- `fixtures/`（固定装置）

**不要动**：`crates/manifest/`、`crates/shim/`（其他票的范围）
