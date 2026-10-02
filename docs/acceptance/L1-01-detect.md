# L1-01 检测引擎 —— 真机验收证据

**采集时间**：2026-10-02
**机器**：MUELSYSE（Windows 11 25H2 build 26200.9457，16 核 / 16.98 GB）
**命令**：`tuoen detect` 与 `tuoen detect --json`
**二进制**：`cargo build -p tuoen-cli --release`（release profile）

原始输出落盘在同一目录：

- `L1-01-detect-machine.txt` —— 人类输出（43 行）
- `L1-01-detect-machine.json` —— `--json` 输出（9,869 字节）

---

## 1. 验收标准逐条对照

| 票据要求 | 结果 | 证据 |
|---|---|---|
| `cargo test --workspace` 全绿 | ✅ **227 passed / 0 failed** | 见下方 §4 |
| clippy `-D warnings` 干净 | ✅ exit 0 | 见下方 §4 |
| `cargo fmt --all --check` 干净 | ✅ exit 0 | 见下方 §4 |
| 本机能跑出真实结果 | ✅ **27 条记录**，六个来源全部命中 | `L1-01-detect-machine.txt` |
| 人工核对至少 3 条 | ✅ 见 §2（核对了 6 条） | 下方 |
| 没有写操作 | ✅ 代码审查 + 只读 trait 设计 | 见 §3 |

---

## 2. 人工核对：取证报告里的已知事实 vs 实际输出

### 2.1 `WindowsApps\python.exe` 必须是 `alias-ghost` ✅

```
python  ?   alias-ghost   ✗ C:\Users\Muelsyse\AppData\Local\Microsoft\WindowsApps\python.exe
```

`evidence` 字段原文：

> PATH 第 1 条（process-only）里有 python.exe，但它是 0 字节的 App Execution Alias（tag 0x8000001b）

**核对通过，并且这条验证了整层设计的必要性**：`Get-Command python` 在这台机器上**成功**，
返回的就是这个 0 字节别名。任何"`Test-Path` 通过就算存在"的实现都会在这里给出错误答案。

**另外两个副产品**：

- 它排在 `PATH` **第 1 条**，而且被判为 `process-only` —— 说明它**不在任何注册表里**
  （取证报告说差的 77 字符是 PowerShell 注入的 MSIX 别名）。这正是 `PathScope::ProcessOnly`
  这一档存在的理由：只读注册表的实现会完全漏掉它。
- 真实的 Python 3.12.10 在**第 3 条**才出现，也就是说**别名排在真 Python 前面** ——
  用户敲 `python` 拿到的是那个幽灵。

### 2.2 nvm4w 的 Node 必须是 `manager-owned` ✅

```
node  24.19.0  manager-owned  ✗ C:\Users\Muelsyse\AppData\Local\nvm
```

`evidence` 字段原文：

> 环境变量 NVM_HOME=C:\Users\Muelsyse\AppData\Local\nvm（user）、NVM_SYMLINK=C:\nvm4w\nodejs（user）、
> NVM_HOME=C:\nvm4w（machine）说明这台机器由 `nvm4w` 管理 node；**同名变量同时存在于用户级与机器级** —— 双重管理腐坏

**核对通过。** 三点值得记录：

1. **版本 `24.19.0` 是从 junction 的目标里读出来的** —— 没有执行任何东西。
2. **`path` 是版本库根目录（`NVM_HOME`）而不是某个 exe** —— 因为"nvm4w 管着 node"这条事实
   的对象是那个版本库，不是某一个版本里的可执行文件。
3. **双重管理腐坏被自动抓到了** —— 取证报告里说 `NVM_HOME` 与 `NVM_SYMLINK`
   **同时存在于 `HKCU\Environment` 和 `HKLM\Session Manager\Environment`**，
   而这里的输出把三个命中全部列了出来。这是设计时没有专门去做的发现，
   它是"两个 scope 都读"这条规则的自然结果。

### 2.3 `java` 与 `javac` 的厂商分裂 ✅（形状与票据原文设想的不同）

```
java  1.8.0_492  executable  C:\Dev\base\JDK\JDK8\bin\java.exe
java  1.8.0_491  executable  C:\Program Files (x86)\Common Files\Oracle\Java\java8path\java.exe
java  1.8.0_491  registered  C:\Program Files\Java\jre1.8.0_491\
java  ?          directory-only  ✗ C:\Dev\base\JDK
```

**分裂是真的，但它的形状是"两条 `java` 记录"而不是"一条 `java` + 一条 `javac`"**：

- `javac` 只有 **JDK8** 那一份（Amazon 的 `1.8.0_492`，路径 `C:\Dev\base\JDK\JDK8\bin\`）
- 而 `java` 在 `PATH` 上被一个 **Oracle 的 Junction** 抢走了（`1.8.0_491`）

也就是说：**用户敲 `java -version` 与 `javac -version` 会得到两个不同厂商的版本。**
这正是要抓的分裂。

**票据里"`javac` 是逻辑工具名"这条被有意改了**（已在 `docs/tickets/L1-01-detect.md` 里写明）：
`java` 与 `javac` 是同一个工具的两个命令。一次真机运行让这条变得必需 ——
`node` 本来会报 4 行（`node.exe` 24.19.0 / `npm.cmd` 11.17.0 / `npx.cmd` 11.17.0 /
`corepack.cmd` 0.35.0），四个版本号挤在同一个工具名下，用户没法回答"我的 Node 是哪个版本"。

### 2.4 `C:\Dev\Tool\apache-maven-3.9.5` 必须被发现 ✅

```
maven  3.9.5  directory-only  ✗ C:\Dev\Tool\apache-maven-3.9.5
```

`evidence` 原文：`本机实测的手工解压工具目录（Maven 在这里）：`apache-maven-3.9.5` 看起来是一个安装目录`

**核对通过。** 这个目录（1,408.8 MB）既不在 winget、也不在 `PATH`、也不在任何注册表 ——
它只被文件系统扫描找到。**版本 `3.9.5` 是从目录名里读出来的**（`version_from_path_hint`），
没有执行任何东西。

### 2.5 `Python311` 幽灵必须被识破，而 `Python312` 不能被误报成幽灵 ✅

```
python  ?          registered-missing  ✗ C:\Users\Muelsyse\AppData\Local\Programs\Python\Python311\
python  3.12.10    registered          C:\Users\Muelsyse\AppData\Local\Programs\Python\Python312\
```

**这一条是本次实现里最有价值的一次修正。** 两个卸载键的形状**一模一样**
（都是 `Python <版本> (64-bit)`、都**没有** `InstallLocation`），
唯一的区别是文件系统：

| | 注册表登记的路径 | 文件系统 | 结论 |
|---|---|---|---|
| Python 3.11.9 | `...\Programs\Python\Python311\` | **不存在** | `registered-missing`（真幽灵） |
| Python 3.12.10 | `...\Programs\Python\Python312\` | 存在 | `registered`（真安装） |

如果只读 ARP，Python 3.12 会被报成幽灵 —— 而它装得好好的。
**一个自己造的假幽灵比漏报更糟**：用户会照着报告去"修"一个没坏的东西，
然后不再信任这个工具。

修法是：没有 `InstallLocation` 时，对 CPython 回退查它**自己**的注册位置
（`HKCU\SOFTWARE\Python\PythonCore\<X.Y>\InstallPath`）—— 然后再**真的去看文件在不在**。
两个案例走的是同一条代码路径，只有文件系统能把它们分开。

### 2.6 注册表里 20 个 Python 卸载键被压成 3 条 ✅

真机上一共有 **20 个**活着的 Python 卸载键（3.12.10 有 11 个、3.11.9 有 9 个 ——
Python 的每个组件与每次修补都会留一个键）。第一版实现把它们**逐条报出来**，
报告里 `python` 占了 20 行，全都是"（无 InstallLocation）"。

现在是 **3 行**：一条 3.12.10（`registered`）、一条 3.11.9（`registered-missing`）、
一条 Python Launcher（`registered-missing`）。

**理由**：二十行重复会把报告淹掉，而报告被淹掉的后果是用户不再读它 ——
那些真正重要的发现（`alias-ghost`、`manager-owned`）就跟着一起被忽略。
合并的键数写进了 `evidence`：

> HKLM\SOFTWARE 下的 11 个卸载键都报告已安装 `Python 3.12.10 (64-bit)`（Python 的每个组件与每次修补都会留一个键）

---

## 3. "没有任何写操作"的证明

**这是代码审查，不是运行时证明** —— 说明写在这里以免被误读成后者。

### 3.1 类型层面的保证

检测引擎的**全部**依赖都是只读 trait（`crates/core/src/detect/context.rs`）：

```rust
pub struct DetectContext<'a> {
    pub fs: &'a dyn FileSystem,        // 只有 inspect / list_dir
    pub registry: &'a dyn Registry,    // 只有 subkeys / values / value / key_exists
    pub env: &'a dyn EnvBlock,         // 只有 list / get
    pub process_env: &'a dyn ProcessEnv,
    pub runner: &'a dyn ProcessRunner,
    pub managed: &'a dyn ManagedStore, // 只有 installed
    ...
}
```

`crates/platform/src/sys.rs` 里**没有任何写注册表的封装** ——
`OpenKey` 包装器只调 `RegOpenKeyExW` / `RegEnumKeyExW` / `RegEnumValueW`，
`RegCloseKey` 在 `Drop` 里。**"只读"是类型事实，不是纪律。**

### 3.2 实际调用面

`detect` 这条路径上会碰机器的调用只有四类：

| 调用 | 位置 | 性质 |
|---|---|---|
| `FindFirstFileW` / `FindNextFileW` | `crates/platform/src/sys.rs` | 只读目录项（属性、大小、reparse tag） |
| `RegOpenKeyExW` / `RegEnumKeyExW` / `RegEnumValueW` | `crates/platform/src/sys.rs` | 只读注册表 |
| `std::fs::read_dir` | `crates/platform/src/fs_facts.rs` | 只读目录 |
| `std::process::Command` + `wait` | `crates/platform/src/process.rs` | 起子进程问版本 |

**没有出现**：`File::create` / `OpenOptions::new().write()` / `std::fs::write` /
`remove_file` / `remove_dir_all` / `create_dir` / `rename` /
`std::env::set_var` / `RegSetValueExW` / `RegCreateKeyExW` / `RegDeleteKeyW` /
`CreateSymbolicLinkW` / `DeviceIoControl`（建 junction 要用它）。

**唯一被写入的是 stdout 与 stderr。**

### 3.3 探测会执行哪些程序

`detect` 会真的起进程问版本。真机上被执行的程序全部来自 `KNOWN_TOOLS` 的
`version_args`（`-v` / `--version` / `-version`），且**只对 `Confidence::Executable`
与 `Confidence::Registered` 的条目**执行。

**`alias-ghost` 与 `registered-missing` 从不被执行** —— 前者会对 0 字节别名启动应用商店，
后者根本不存在。这条有专门的用例钉住（`real_machine_acceptance.rs` 的
`an_app_execution_alias_is_recognised_from_real_ntfs`，断言 `alias-ghost` 的
`version` 必须是 `null`）。

### 3.4 运行时观察

`detect --json` 连续两次运行的输出**逐字节相同**（契约测试
`json_output_is_byte_stable_across_runs` 钉住这一点）。任何真实的写入都会改变
下一次检测的结果（多一个工具、少一个工具、`PATH` 变长）。

**注意这一条的强度**：它证明的是"两次运行之间没有可观测的变化"，
**不是**"没有写操作"。真正的证明是 §3.1 的类型保证与 §3.2 的调用面清点。

---

## 4. 门禁结果（实测）

```
$ cargo test --workspace
  227 passed / 0 failed   （9 个测试目标）

$ cargo clippy --workspace --all-targets -- -D warnings
  exit 0

$ cargo fmt --all --check
  exit 0
```

测试分布：

| 目标 | 数量 | 说明 |
|---|---|---|
| `tuoen-cli` bin 单测 | 26 | 参数解析、视图、宽度计算 |
| `catalog_contract` | 13 | `catalog` 的进程边界契约（ticket #3） |
| `cli_contract` | 10 | `list` 的进程边界契约（ticket #2） |
| `detect_contract` | 13 | **`detect` 的形状契约（不读真机）** |
| `real_machine_acceptance` | 5 | **`detect` 的真机验收（有意读真机）** |
| `tuoen-core` | 47 | 检测引擎、spec 表、固定装置适配 |
| `tuoen-manifest` | 65 | 两层 schema、许可证门禁、版本解析 |
| `tuoen-platform` | 46 | Win32 只读原语、reparse、注册表、进程 |
| `tuoen-shim` | 2 | shim spec 形状 |

---

## 5. 性能

```
$ tuoen detect --json        # 全量，含版本探测
  1.46 秒
```

其中六个来源的结构查询（`PATH` 定向探测 × 48 条、三个 hive 的 ARP、
App Paths、两个扫描根）是主要开销，版本探测大约 20 次进程启动。

**定向探测而不是列目录**是关键：`PATH` 有 48 条，其中若干是含数千文件的
`C:\Windows\System32` 这类目录。逐条 `list_dir` 再逐个 `inspect` 会让一次
`detect` 变成几万次系统调用。而"Windows 怎么找 `node.exe`"这件事本身就是
**按名字在目录里找** —— 定向探测不仅更快，它也更贴近真实语义。

---

## 6. 本次实现发现并修掉的真机问题（每条都有证据）

| # | 现象 | 根因 | 修法 |
|---|---|---|---|
| 1 | `jar.exe` 的版本被解析成 `Pack200` | 真机 `jar -version` 第一行是 `Pack200 1.8.0_492`；且更早一行是 `[0.003s][warning][cds] …` | 加 `Pack200 ` 前缀；**跳过以 `[` 开头的警告横幅行** |
| 2 | `git` 报"发现但版本未知" | 这台机器的 Git 是 **VFS for Git** 构建，`git --version` 走 **stdout**；而标准 Git for Windows 走 stderr | `VersionStream::Both` |
| 3 | `wsl` 报"发现但版本未知" | `wsl --version` 输出 **UTF-16LE**；按 UTF-8 读是 `W\0S\0L\0`，每个 token 都被 NUL 隔断 | 输出规范化：奇数位大量 NUL 时按 UTF-16LE 还原 |
| 4 | 同上，修了 3 仍然取不到 | `ProcessOutcome::combined()` 在 stdout 与 stderr 之间插一个 `\n`，stdout 是 UTF-16 且 stderr 为空时**总长度是奇数**，早先的实现对奇数长度直接放弃 | 削掉尾部孤立字节再判 |
| 5 | Python 3.12.10 被报成幽灵 | 那个卸载键没有 `InstallLocation`，而只读 ARP 就判幽灵 | 对 CPython 回退查 `HKCU\SOFTWARE\Python\PythonCore\<X.Y>\InstallPath`，**再真的看文件在不在** |
| 6 | ARP 版本号是垃圾（`python 3.12.10150.0`、`dotnet 64.104.50421`、`java 8.0.4910.10`、`git 2.53.0.0.7`） | `DisplayVersion` 是**安装器的内部版本号**，不是工具版本 | 版本改为问真实文件；`DisplayVersion` 只进 `evidence`，并在与真实版本不一致时两个都写出来 |
| 7 | 20 个 Python 卸载键占了 20 行 | 逐条报告 | 按（工具 + 置信度 + 路径 + `DisplayVersion`）合并，键数写进 evidence |
| 8 | `path` 字段里是一句中文说明 | 没有 `InstallLocation` 时塞了 `（无 InstallLocation；卸载键 {…}）` | 改成 `<无 InstallLocation，卸载键 {…}>` 占位符；说明留在 `evidence` |
| 9 | ARP 条目被标成 `executable`，但它们是**目录**且不在 `PATH` 上 | 六层里没有合适的一层 | 新增第七层 `registered` |
| 10 | `node` 报了 4 行（版本 24.19.0 / 11.17.0 / 11.17.0 / 0.35.0） | 次要命令与主命令混在一起报 | `ExecutableName::primary`：只报主命令；次要命令仍参与遮蔽判定但不进报告 |

---

## 7. 已知的、**没有**在本次解决的问题

诚实列出，避免下一票重复踩：

1. **同一安装被两个来源各报一次。** 例如 `java` 的 `C:\Program Files\Java\jre1.8.0_491\`
   同时被 ARP（`registered`）与 `PATH`（`executable`，经 Oracle Junction）报出来。
   这是**有意的**（两个来源的 `evidence` 不同，都是真实发现），但当工具很多时
   `registered` 这一层会变成噪音。**留给 `doctor` 那一票处理** —— 它的职责就是
   把这种重复判成 finding。
2. **`dotnet` 的 `--version` 失败**（本机没有 SDK），所以版本是 `?`。
   这是正确行为（不编版本），但报告里看不出"为什么问不到"。
3. **`wsl` 报了 4 条**（`registered-missing` / `executable` ×2 / `alias-ghost` / `registered`）。
   同样属于第 1 条。
4. **`HKCU\SOFTWARE\Python\PythonCore` 的绝对路径处理是特例。**
   `RegHive::resolve` 里加了一个 `Python\` 前缀走绝对解析。这有点丑，
   但目前只有 CPython 有官方的注册表位置，别的工具没有。
5. **`fixtures/detect/*.toml` 还没有落盘。** `tuoen_platform::fixture::MachineFixture`
   支持从 TOML 反序列化，但目前的用例都是在 Rust 里构造的。
   等需要跨 crate 共享同一份机器描述时再落盘。
