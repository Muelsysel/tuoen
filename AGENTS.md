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
- 归档：系统自带 `tar.exe` 是 bsdtar 3.8.8（zip/tar.gz/tar.xz/tar.zst/tar.bz2 都能走它），**`.7z` 是唯一缺口**

## 构建这台机器（三个实测发现，别重新踩）

**本机没有 MSVC Build Tools —— `link.exe` 不存在。** 所以只能走 `x86_64-pc-windows-gnu`。以下三条都是实测出来的，不是推断：

1. **`rust-toolchain.toml` 必须写完整 host 三元组**（`channel = "stable-x86_64-pc-windows-gnu"`）。只写 `"stable"` 时 rustup 按 `Default host`（= msvc）解析，host 侧的构建脚本与 proc-macro 会失败：`error: linker link.exe not found`。
2. **rustup 的 `rust-mingw` 组件不提供可用的 `dlltool`。** `windows-sys` 在 GNU target 上需要它生成 import library，而 rustup 自带的是垫片（旁边的 `GCC-WARNING.txt` 说那个 gcc 只能当链接器用），会报 `dlltool could not create import library …: CreateProcess`。已装一份用户级 MinGW-w64 在 `C:\Users\Muelsyse\.local\toolchains\mingw64`（WinLibs，不需要管理员）。
3. **rustc 在 GNU target 下不认 `DLLTOOL_<target>` 变量，它就在 `PATH` 上找 `dlltool.exe`**；而 cargo 的 `[env]` 对已存在的变量默认不生效（`force = true` 时也不是前置而是覆盖）——两种写法都实测失败。所以 MinGW 的 `bin` 在 `HKCU\Environment\Path` 里（只读、非提权、`REG_SZ` 保持不变），linker 路径留在 `.cargo/config.toml` 的 `[target.x86_64-pc-windows-gnu]`。

`cargo` 与 `rustc` **不在**默认 `PATH` 上，用 `$env:USERPROFILE\.cargo\bin`。

## 遇到实现问题的第一参考对象

**`mise`**（Rust、MIT、34.5k★、周更）——尤其是它的 Windows shim 处理、PATH 管理、`bootstrap` 资源抽象，以及 issue 区对已知 Windows 坑的记录。这是项目所有者明确指定的。
