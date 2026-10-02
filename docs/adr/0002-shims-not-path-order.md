# ADR-0002: 用 shim 暴露命令，不靠 PATH 顺序

**状态**: 已接受（2026-10-02）
**来源**: `docs/DESIGN.md` 决策 10、12；本机实测

## 背景

需要让 `node` / `java` / `python` 这些命令解析到我们管理的版本。直觉方案是把我们的目录插到 `PATH` 前面。

**实测推翻了它**：

```
进程 PATH 顺序 = [进程注入项] ; HKLM Path ; HKCU Path
                              ↑ 首条是机器条目
机器 Path 978 字符 / 32 条  +  用户 Path 725 字符 / 15 条  =  1781 字符 / 48 条
```
**用户级工具只能往尾部追加，因此在名字冲突上永远输给机器级条目。**

本机就是活样本：机器级 `PATH` 已有 `C:\nvm4w\nodejs`（nvm4w 的 node）与 `C:\Program Files (x86)\Common Files\Oracle\Java\java8path`。我们的 shim 装在用户级，**用户敲 `node` 命中的仍然是 nvm4w**。

## 决策

**用真 `.exe` shim 暴露命令；只发 `.exe`，绝不发 `.cmd` / `.ps1`。**

## 后果

- 命令暴露不再依赖 `PATH` 顺序，因此不会被其他安装器打乱。
- **`.cmd` 被排除有独立理由**：Node ≥18.20.2 / 20.12.2 / 21.7.3 之后，`.cmd` 文件**无法被 spawn**（CVE-2024-27980 的修复，nodejs/node#52681）。Scoop 每个 shim 写 4 个文件（`.exe` + `.shim` + `.cmd` + `.ps1`），我们只写 `.exe`。
- **遮蔽是必须处理的产品问题，不是边缘情况**：检测到机器级条目遮蔽我们的 shim 时，明确告知"这 N 个条目遮蔽了我们"，由用户选择是否提权接管。**绝不静默改写系统状态**（决策 3）。
- shim 在关键路径上，**性能是设计约束**：实测原生 shim 约 33–36ms vs C# 约 86ms。因此 shim 必须是独立的最小二进制（决策 29），且版本解析在生成时写死（决策 11），不做运行时配置读取。

## 被否决的方案

- **`PATH` 顺序技巧**：见上，结构上不可能赢。
- **`.cmd` / `.ps1` shim**：无法被 Node spawn，且脚本 shim 有引号转义与退出码透传的坑。
- **复用 Scoop 的 shim 目录约定**：继承它的限制与 bug 面；且会与 Scoop 自身冲突。

## 参考

Scoop 的参考 shim 解决了这些具体问题，值得借鉴：参数转发（**切原始 `GetCommandLineW`，而不是重新引号化 `argv`**）、Ctrl-C 透传、job-object 子进程清理、用 `SHGetFileInfoW` 区分 GUI/控制台、`ERROR_ELEVATION_REQUIRED`(740) 时回退 `ShellExecuteEx`。
