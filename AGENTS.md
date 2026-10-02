# tuoen · 拓境 — Agent 工作约定

本文件是本仓库的 agent 约定入口。**读它，然后读它指向的文件。**

## 项目一句话

`tuoen`（拓境）是一个面向 Windows 的开源开发环境管理器，招牌能力是**整机开发状态（dev-state）的捕获与还原**，而非又一个包安装器。Rust 实现，MIT OR Apache-2.0。

## 先读这个，再动任何东西

**`docs/DESIGN.md` 是必读，不是参考。** 它是七轮设计拷问的决策日志，含 **35 条带理由的决策**。它是本仓库里若干"本来很合理"的设计被**禁止**的原因——例如：

- **绝不用 `setx` 写 `PATH`**（1024 字符裁剪 + 永久展开 `%VAR%`）
- **绝不断言 symlink 可创建**（未提权且未开 Developer Mode 时文件与目录都失败）
- **绝不发 `.cmd` / `.ps1` shim**（Node ≥18.20.2 后无法被 spawn）
- **绝不改写 winget / Scoop 的 `PATH` 与状态**
- **绝不捕获密钥材料**（只捕获意图）
- **绝不自动清理幽灵条目**（只报告）

违反其中任何一条之前，先读它对应的决策与理由。

## Agent skills

### Issue tracker

Issues, specs and tickets live as GitHub issues in `Muelsysel/tuoen`, operated through the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five canonical triage roles keep their default label strings (`needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`), plus `spec` and `wayfinder:*`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and one `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## 开发流程

本仓库遵循 mattpocock/skills 的主流程。完整路由见 `/ask-matt`。核心链条：

```
/setup-matt-pocock-skills  →  /grill-with-docs  →  /to-spec  →  /to-tickets  →  /implement-spec
```

**上下文卫生**：`/grill-with-docs` → `/to-spec` → `/to-tickets` 必须在**同一个不中断的上下文窗口**里完成（不要在 `/to-tickets` 之前 compact 或 clear），这样拷问、spec 和票据建立在同一套思考上。之后每个 `/implement` 才开新窗口。

## 领域语言

术语以 `GLOSSARY.md` 为准。**不要漂移到它明确避免的同义词**——例如"票据"（ticket，来自 `/to-tickets` 的垂直切片）与"issue"（tracker 上的条目）在本仓库是不同概念；"junction" 与 "symlink" 是**不可互换**的两种 reparse point。

## 中文优先

- 默认界面中文，中文文档优先，英文 README 并行
- **但 `--json` 输出必须稳定、不本地化**——脚本化与 GUI 都依赖它
- **永远不要匹配错误文本**（本机 zh-CN，PowerShell 的符号链接报错是本地化的：`此操作需要管理员权限。`），要匹配 HResult / Win32 码

## 平台现实

代码运行在 Windows 上，且**默认非提权**。动手前必须知道的地面事实（详见 `docs/DESIGN.md` §2）：

- 进程 `PATH` = 机器条目在前、用户条目在后 → 用户级工具**永远输掉名字冲突**，只能靠 shim 抢名字
- `cmd.exe` 在 `PATH` 超过 **8191 字符**后**完全忽略**它（整条 PATH 一次性全部失效）；`setx` 在 **1024** 处裁剪
- 相对路径**永远**受 `MAX_PATH`(260) 限制——`\\?\` 无法加前缀
- `PATH` 上 29% 的条目含空格，最糟的同时含空格和版本号
- 归档：系统自带 `tar.exe` 是 bsdtar 3.8.8（zip/tar.gz/tar.xz/tar.zst/tar.bz2 **以及 7z** 都能走它——7z 那条是**实测**的，见 `research/BSDTAR_SAFETY_MEASURED.md`）
- **bsdtar 对路径逃逸可靠（`..`/symlink/硬链接/设备全拒，且退出码变 1），但对名字的危险形态不可靠**：`CON` 真的会被创建且普通路径看不见、`trailing.` 删不掉、`:` 被静默改名、大小写碰撞静默覆盖。所以解压必须自己校验条目名

## 构建这台机器（三个实测发现，别重新踩）

**本机没有 MSVC Build Tools —— `link.exe` 不存在。** 所以只能走 `x86_64-pc-windows-gnu`。以下三条都是实测出来的，不是推断：

1. **`rust-toolchain.toml` 必须写完整 host 三元组**（`channel = "stable-x86_64-pc-windows-gnu"`）。只写 `"stable"` 时 rustup 按 `Default host`（= msvc）解析，host 侧的构建脚本与 proc-macro 会失败：`error: linker link.exe not found`。
2. **rustup 的 `rust-mingw` 组件不提供可用的 `dlltool`。** `windows-sys` 在 GNU target 上需要它生成 import library，而 rustup 自带的是垫片（旁边的 `GCC-WARNING.txt` 说那个 gcc 只能当链接器用），会报 `dlltool could not create import library …: CreateProcess`。已装一份用户级 MinGW-w64 在 `C:\Users\Muelsyse\.local\toolchains\mingw64`（WinLibs，不需要管理员）。
3. **rustc 在 GNU target 下不认 `DLLTOOL_<target>` 变量，它就在 `PATH` 上找 `dlltool.exe`**；而 cargo 的 `[env]` 对已存在的变量默认不生效（`force = true` 时也不是前置而是覆盖）——两种写法都实测失败。所以 MinGW 的 `bin` 在 `HKCU\Environment\Path` 里（只读、非提权、`REG_SZ` 保持不变），linker 路径留在 `.cargo/config.toml` 的 `[target.x86_64-pc-windows-gnu]`。

`cargo` 与 `rustc` **不在**默认 `PATH` 上，用 `$env:USERPROFILE\.cargo\bin`。

## 会启动进程的代码：三条铁律（两次事故换来的）

本仓库已经有**两次**因为探针/测试启动进程而把整机拖垮的记录。两次都不是"逻辑写错"，
而是**失效模式的代价不对称**：一个名字、一个字节的差错，换来整机失去响应。

1. **绝不允许任何程序启动它自己。** 第一次：一个测试让 shim 的**落盘路径与目标路径是
   同一个文件**，生成出的转发器"目标是它自己"，运行一次就无限自我启动，堆到 **20580 个
   进程**。第二次：Ctrl-C 探针的宿主"fork 自己"进入子模式，而 `wide()` 已经带了 NUL
   结尾、参数被拼在了那个 NUL **后面**，于是子进程收到零个参数、回到宿主模式又 fork
   一次 —— **409 个进程**，每个还带着一个不可见控制台和继承来的 stdout 管道。
   现在的做法是：宿主与执行者是**两个不同的二进制**，宿主在启动前断言"执行者不是我"。
   需要两个身份时，写两个二进制，不要重跑自己。

2. **构造命令行只留一个函数，参数必须拼在结尾的 NUL 之前。** 第二次事故的根因就是
   "半成品缓冲 + 自己补 NUL"。让调用方拿不到半成品，这个 bug 就没有藏身处。
   同类危险还有：前缀参数里混进 U+0000 会把命令行在那里截断，目标带着比预期少的参数
   启动且不报错 —— `ShimSpec::validate` 现在直接拒掉它。

3. **跑之前先算最坏情况下的进程数，给它上限，跑完清场核对。** 探针要能在"转发器死了而
   子进程还活着"这种局面下自己收场（`WaitForSingleObject` 带超时，不用 `INFINITE`）。
   **一条不能失败的测量不是测量。**

## 会改用户系统状态的代码：五条规矩（票据 #8 定下来的）

`PATH` 是本项目里**唯一**一处"一次调用就能让整台机器所有命令失效"的地方，所以动它的代码
比别处多守五条。它们的共同点：**不是"写得更小心"，而是把危险操作围在证据里**。

1. **绝不调用 `setx`。** 它在 **1024** 字符处静默裁剪，并把值里所有 `%VAR%` **永久展开**
   成字面量。写 `PATH` 只有一条路：`RegSetValueExW` 整条写回 + 广播
   `WM_SETTINGCHANGE`，且**值含 `%` 才用 `REG_EXPAND_SZ`**（否则保留原类型，再否则 `REG_SZ`）。
   验收脚本用 grep 钉住这件事（判据是**带引号**的 `setx` 字面量 —— 源码里到处都用反引号
   写 `` `setx` `` 来警告不许调用它）。

2. **读用户的环境变量不许展开。** 低层走 `RegEnumValueW`（它**从不**展开 `REG_EXPAND_SZ`）。
   展开是**不可逆的信息损失**：只有不展开读，才区分得出"值本来就是这样"与"被 `setx`
   展开过"。推论：含 `%` 的条目**不许**判成"失效"—— 我们没展开它，答不了的题不许猜。

3. **先出计划，再落盘；`--dry-run` 走同一套计划代码。** 另写一条只读分支的话，
   dry-run 通过只能证明那条分支对。验收里钉这件事的办法是断言
   `run(dry_run=true)` 与 `run(dry_run=false)` 产出**同一个计划对象**。
   计划与现状一致时**不写、也不广播** —— 广播是全局副作用（打到每一个顶层窗口）。

4. **危险的探针必须自证它没碰不该碰的东西。** 真机探针一边写 `TUOEN_PATH_PROBE`
   做类型往返，一边在开头与结尾各快照一次两个作用域的 `Path`（类型 + 原始字节），
   `before == after` 必须为真并写进机器可读的 `SUMMARY` 契约行。**声称没碰 ≠ 证明没碰**，
   而且这份证据要能被机器检查。测试同理：`cargo test` 永远不许碰真实
   `HKCU\Environment` / `HKLM` —— 需要假注册表就给假注册表，**不给出厂二进制留后门开关**。

5. **测试断言不许依赖机器状态，只能断言"跑完前后没变"。** "真实的 `%LOCALAPPDATA%\tuoen\shims`
   不存在"这种断言是**机器状态**，不是被测代码的性质：任何人真的用过 `tuoen shim add`
   都有这个目录，于是它会在最不该红的时候红（票据 #8 的真机验收就把它踩红了一次）。
   正确形状是"跑之前列一遍、跑之后再列一遍、逐项相同"——而且**要往被测代码会写的那个目录里
   看一层**：只列顶层的话，"在真实 shim 目录里写了一个 shim"这种最严重的事故看不见，
   因为那个目录名本来就在那儿。这是决策 50 的第二次教训：第一次是"断言不会失败"
   （假的通过），这一次是"断言会被误报"（假的红）。

## 遇到实现问题的第一参考对象

**`mise`**（Rust、MIT、34.5k★、周更）——尤其是它的 Windows shim 处理、PATH 管理、`bootstrap` 资源抽象，以及 issue 区对已知 Windows 坑的记录。这是项目所有者明确指定的。
