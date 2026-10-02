## 目标

实现 **检测引擎（detection engine）**：把"这台机器上装了什么开发工具"读成结构化记录，**每条都带来源与置信度**。这是 L1 四个能力（捕获 / 诊断 / 还原 / pin）共用的地基，所以这一票单独切出来。

## 必须实现的

### 1. `tuoen detect --json` 命令（内部能力，但必须有 CLI 出口以便端到端验收）

输出 `Vec<DetectedTool>`，字段固定：

```
name          逻辑工具名（node / python / java / javac / git / ...）
version       探测到的版本（可能为空 —— 空表示"发现但无法确定版本"）
path          主路径（可执行文件或安装根目录）
source        tuoen | nvm4w | winget | registry-arp | app-paths | filesystem-scan | path-resolution
confidence    见下
manager       第三方版本管理器名（如 nvm4w），无则为空
evidence      人类可读的一句话，说明这条是怎么被发现的
```

### 2. 六个 `confidence` 层级，判据必须精确

| 值 | 判据 |
|---|---|
| `managed` | 存在 tuoen 自己的安装记录（L0 完成后才有） |
| `executable` | 在 `PATH` 上能解析到，目标**不是** App Execution Alias，文件真实存在且**大小 > 0** |
| `registered-missing` | 注册表声称已安装，但文件不存在 → 幽灵条目 |
| `directory-only` | 发现目录，未在任何注册表 / `PATH` / `App Paths` 里注册 |
| `alias-ghost` | 是 App Execution Alias（**reparse tag `0x8000001b`、长度 0**），`Test-Path` 通过但**不是文件** |
| `manager-owned` | 由第三方版本管理器管理（只读采纳） |

**`alias-ghost` 必须独立存在。** 本机实测：`WindowsApps\python.exe` 是 0 字节、tag `0x8000001b`，而 `Get-Command python` **成功**并排在 `PATH` 最前。任何"`Test-Path` 通过就算存在"的实现都会在这里给出错误答案。

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

**必须有的用例**：

- 六个 `confidence` 各一个 fixture 用例，**`alias-ghost` 必须有**（构造 tag `0x8000001b` + 长度 0 的 fixture）
- 幽灵条目：注册表有键、文件不存在 → `registered-missing`，且**不产生任何删除动作**
- 版本探测：stdout 与 stderr 各一个用例（模拟 `node -v` 与 `java -version` 的差异）
- 版本探测超时：模拟一个不返回的工具 → 记录"发现但版本未知"，**不挂住**
- App Paths：一条只在 App Paths 里、不在 `PATH` 里的条目能被发现
- 遮蔽：机器级条目与用户级同名可执行文件 → 记录解析到哪个、以及被谁遮蔽

## 验收标准

- `cargo test --workspace` 全绿，clippy `-D warnings` 干净
- `cargo run -p tuoen-cli -- detect --json` 在**本机**能跑出真实结果，且**人工核对至少 3 条**：`java` 与 `javac` 解析到**两个不同厂商**的安装；`WindowsApps\python.exe` 被标为 `alias-ghost`；nvm4w 的 Node 被标为 `manager-owned`
- 上述人工核对的结果**贴进本 issue 的评论**（这是这一票的"证据"，不是可选步骤）
- 没有任何写操作（可用文件系统监视或代码审查证明）

## 写入范围（advisory）

- `crates/core/`（检测引擎逻辑）
- `crates/platform/`（注册表 / reparse point / 版本探测的平台原语）
- `crates/cli/`（`detect` 子命令）
- `tests/`（fixture 与端到端用例）
- `fixtures/`（固定装置）

**不要动**：`crates/manifest/`、`crates/shim/`（其他票的范围）
