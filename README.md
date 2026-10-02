# tuoen · 拓境

> **拓境 — 让你的 Windows 开发环境可搬运、可复现。**
>
> *Tuoen — capture, carry, and rebuild your Windows dev environment.*

[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#许可证)

**状态：设计阶段（design phase）。尚无可用代码。** 完整设计见 [`docs/DESIGN.md`](docs/DESIGN.md)。

---

## 它解决什么问题

换一台电脑，或者把开发环境交给同事复现，你会遇到这些事：

- **PATH 是一团历史遗留物** —— 重复条目、指向已卸载软件的失效路径、手写时拼错的目录、硬编码了旧用户名的路径。没有任何工具把它当作资源来管理。
- **"我装的是哪个 Java/Python"没有唯一答案** —— 同一个工具可能有多个安装来源（版本管理器、官方安装器、手工解压、IDE 自带），而注册表和 `PATH` 给出的答案互相矛盾。
- **注册表声称已安装，文件却不存在** —— 卸载只做了一半，留下"幽灵条目"，而所有包管理器都照样报告它已安装。
- **配置和密钥混在一起** —— 备份配置文件会连带外泄 token；而真正无法复制的密钥（凭据管理器、DPAPI）又必须手动重配。
- **没有人能捕获一整台机器的开发状态** —— `winget export` 只导包 ID 并对无法匹配项给警告，`scoop export` 只导应用，`chezmoi` 管文件不管机器。

`tuoen` 的目标是**把一台 Windows 机器的开发状态变成机器可读、可审查、可重建的东西**：
先 `plan`，再 `diff`，你确认后才 `apply`。

## 核心能力（规划中）

| 能力 | 说明 |
|---|---|
| **整机状态捕获与还原** | 工具链、环境变量、PATH 结构、配置文件清单、WSL、全局包 —— 逐项标注**来源与置信度** |
| **受管的 PATH** | 所有者、顺序、重复与死条目检测、**长度预算告警**（`cmd.exe` 在 8191 字符后完全忽略 `PATH`）、声明式移除 |
| **多版本共存与项目级 pin** | 单文件 `tuoen.toml` 声明整条工具链；`tuoen shell` 永远可用，自动切换需 `tuoen trust` 一次 |
| **提权作为一等资源** | 逐项 scope、一次同意、明确的非管理员降级路径，绝不静默改写系统状态 |
| **环境体检诊断** | 报告 PATH 冲突、重复安装、版本漂移、幽灵条目、用户名依赖 |
| **镜像加速** | 源优选 + 用户自定义镜像模板；**不托管任何二进制** |
| **离线 bundle** | 元数据 + 厂商 URL + 哈希；自包含归档**只装许可允许再分发的** |
| **全局包管理** | npm -g / pip 的捕获与还原，**按工具版本隔离** |

**与 winget / Scoop 的关系**：只读检测它们装了什么，**绝不改写它们的 `PATH` 与状态**。

**不做的事**：不重新分发 Oracle JDK 8/11/17、不重新分发 MSVC（不可再分发）；不默认启用 Chocolatey 社区仓库（其条款自 2027-01-01 起禁止组织内直接使用，且明确覆盖第三方工具）。

## 设计原则

1. **绝不用 `setx` 写 `PATH`** —— 文档写 1024 字符上限且内容是"被裁剪后应用"（静默数据丢失），并会永久展开 `%VAR%` 引用。直接写注册表并广播 `WM_SETTINGCHANGE`。
2. **Junction 优先，symlink 可选** —— 未提权且未开 Developer Mode 时 symlink 创建会失败，而 junction 可以。
3. **只发真 `.exe` shim** —— 不发 `.cmd`/`.ps1`（`.cmd` 在 Node ≥18.20.2 后无法被 spawn）。
4. **捕获意图，绝不捕获材料** —— 记录"这里有个凭据，需要你手动重配"，不复制 token、密文或 DPAPI blob。
5. **破坏性动作永远由用户显式触发** —— 幽灵条目只报告，不自动清理。
6. **`--json` 输出稳定、不本地化** —— 界面可以中文，接口不行。

## 技术栈

**Rust**（Cargo workspace 多 crate）· **Tauri v2**（GUI，与 CLI 共用同一套类型）· **MIT OR Apache-2.0**

选 Rust 的决定性理由是**分发与自举**：一个用来管理 Node/Python 的工具，让用户先装 Node 才能用它是逻辑循环。

## 构建

> 尚未可用 —— 仓库当前处于设计阶段，代码骨架待建。

计划中的构建前置条件：Rust 工具链、MSVC Build Tools（GUI 需要）、Node.js（仅 GUI 前端需要）。

## 文档

| 文件 | 内容 |
|---|---|
| [`docs/DESIGN.md`](docs/DESIGN.md) | 完整设计规格：35 条决策及其理由、平台硬约束、许可与再分发规则 |
| [`docs/CODE_SIGNING.md`](docs/CODE_SIGNING.md) | 代码签名政策（SignPath Foundation 申请所需） |
| [`docs/UNSIGNED_BUILD.md`](docs/UNSIGNED_BUILD.md) | **当前版本未签名，运行时 Windows 会警告 —— 先读这个** |

## 贡献

贡献指南待补（`CONTRIBUTING.md`）。当前阶段：欢迎在 Issues 中讨论设计。

## 许可证

以 **MIT OR Apache-2.0** 双许可发布，与 Rust 生态惯例一致。你可以任选其一：

- [`LICENSE-MIT`](LICENSE-MIT)
- [`LICENSE-APACHE`](LICENSE-APACHE)

## 致谢

- 免费代码签名由 [SignPath.io](https://signpath.io) 提供，证书由 [SignPath Foundation](https://signpath.org) 签发。
- 设计阶段的平台约束与竞争格局调研参考了 [mise](https://mise.jdx.dev)、[Scoop](https://scoop.sh)、[aqua](https://aquaproj.github.io)、[vfox](https://vfox.dev)、Python 3.14 官方安装器的公开文档与源码。

---

## English

**Tuoen** is an open-source Windows developer-environment manager. Its headline capability is
**whole-machine dev-state capture and restore** — not another package installer.

Plan the change, review the diff, then apply. Managed `PATH`, multi-version coexistence with
per-project pinning, planned elevation, mirror acceleration, offline bundles, and npm/pip global
package capture.

**Status: design phase — no working code yet.** See [`docs/DESIGN.md`](docs/DESIGN.md).

Built in Rust · GUI via Tauri v2 · Licensed MIT OR Apache-2.0
