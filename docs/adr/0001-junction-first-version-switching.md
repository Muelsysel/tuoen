# ADR-0001: Junction 优先做版本切换，symlink 作为可选路径

**状态**: 已接受（2026-10-02）
**来源**: `docs/DESIGN.md` 决策 9；本机实测（Windows 11 25H2 build 26200，Developer Mode 关闭，进程未提权）

## 背景

版本切换需要"每个工具一个稳定入口指向当前版本"。候选机制：目录符号链接、目录联接（junction）、`PATH` 前置插入、可执行 shim。

我最初（设计第 2 轮）推荐"符号链接翻转"，理由是它在语义上最"正确"。**随后本机实测推翻了这条推荐**：

```
token 中无 SeCreateSymbolicLinkPrivilege
New-Item -ItemType SymbolicLink   → 文件与目录都失败（UnauthorizedAccessException）
mklink /D 与 mklink               → "You do not have sufficient privilege"（exit 1）
New-Item -ItemType Junction       → 成功
mklink /H（硬链接）                → 成功
```
微软文档明确：`SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`(0x2) **只在 Developer Mode 已开启时有效**——传这个 flag **不是**绕路方案。

## 决策

**Junction 优先，symlink 作为可选路径**（检测到 Developer Mode 已开启或进程已提权时才使用 symlink）。

## 后果

- **不需要管理员权限即可切换版本**。这条几乎决定了它必须是默认：一个管理开发工具的工具，如果每次装东西都要 UAC，用户会立刻放弃。
- 与决策 3（只读检测 winget/Scoop）的隔离性一致：我们的机制完全自持。
- **必须处理一个 symlink 方案没有的问题**：同一条 `PATH` 上可能存在两种 reparse 类型（本机就有：Oracle 的 `java8path` 是 junction，nvm4w 的 `C:\nvm4w\nodejs` 是 symlink）。诊断功能必须把这个差异讲清楚。
- junction 的目标名可能嵌安装序号（本机 `java8path_target_1783390`）——**必须记录目标，不能猜测**。
- 硬链接不作为通用机制：仅 NTFS、仅文件、必须同卷，且**当应用用 rename 替换自己的文件时会断**。

## 被否决的方案

- **纯 symlink**：给用户加"必须开 Developer Mode"的前置条件，且未提权时直接失败。
- **`PATH` 前置插入**：用户级条目在 `PATH` 上永远排在机器级之后（实测：进程 `PATH` 首条为机器条目），因此**在名字冲突上永远输**；且 `PATH` 顺序极易被其他安装器打乱。
- **硬链接作为通用机制**：见上，局限太多。

## 参考

- 先例：Scoop 的 `link_current` 也把 `<app>/current` 做成 **JUNCTION**（不是 symlink），并用 `attrib +R /L` 标记只读，另有 `NO_JUNCTION` 配置逃生口。
- 反例：**vfox 用目录 symlink + `PATH` 优先级**，与上述实测直接冲突——**在 Windows 上它需要提权或 Developer Mode。抄它的设计前必须先测。**
