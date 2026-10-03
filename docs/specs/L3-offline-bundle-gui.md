# spec: L3 重资产（离线 bundle + Tauri v2 GUI）

> 本 spec 是 `/to-spec` 的产物，来自 `docs/DESIGN.md` 的决策 18 / 19 / 29 / 30 / 32 / 34 / 35
> 与 L3 侦察期的**真机实测**（下面每个数字都标了来源）。
> **术语以 `GLOSSARY.md` 为准**；与 `docs/DESIGN.md` 冲突时以 ADR 为准。
> 前序 spec：L0 `docs/specs/L0-install-engine.md`、L1 `docs/specs/L1-dev-state.md`、L2 `docs/specs/L2-global-packages.md`。

---

## Problem Statement

L0/L1/L2 合起来已经能做到：**在新机器上，只要有网络，就能把开发环境重建出来**。
但本项目的目标人群（DESIGN §0）恰好包含"**内网 / 离线机器**"这一半 —— 对它们，
现在的 `tuoen` 什么也做不了：`restore` 的每一步都要下载，而下载在离线机器上必然失败。

同时，`tuoen` 的能力现在只有命令行。而"整机开发状态"这件事的信息量很大
（本机实测：`capture` 八份文件、49 条 PATH 行、32 个环境变量、27 个工具、29 条体检发现），
纯文本表格读起来费力，而**计划**（会写什么、写到哪、遮蔽了谁）尤其需要"看"而不是"读"。

L3 侦察期的四条实测事实决定了离线 bundle 的形状：

1. **`npm pack <pkg>@<ver> --pack-destination <dir> --offline` 会失败**：
   `npm error code ENOTCACHED` + `request to https://registry.npmjs.org/pnpm failed: cache mode is
   'only-if-cached' but no cached response is available`（632 ms，退出码 1）——
   **即使那个包已经装在全局树里**。去掉 `--offline` 就成功（2443 ms，产出 `pnpm-11.21.0.tgz`，8 799 855 B）。
   ⇒ **bundle 的"取材"必须在有网络的机器上做**，这一点必须写进产品的话里，不能假装全离线。
2. **从本地 tarball 离线安装是可行的**：`npm install -g --offline <tgz>` → `added 1 package in 5s`
   （4899 ms，退出码 0），`npm ls -g --json --depth=0 --offline` 看得到它。
3. **pip 的取材与消费都是干净的**：`pip download --dest <dir> --no-deps pygments==2.21.0`
   → 1461 ms，`pygments-2.21.0-py3-none-any.whl`（1 250 147 B）；
   `pip install --user --no-index --find-links <dir> pygments==2.21.0` → `Successfully installed`（5623 ms）。
   ⇒ 目标机器上**完全不需要网络**。
4. **L0 的下载缓存本身就是内容寻址的**：`%APPDATA%\tuoen\cache\sha256\<前两位>\<完整 sha256>`
   + 同名 `.meta.json`（228 B）。本机实测 4 份归档 / **135 905 531 B**（node 等）。
   ⇒ 工具归档可以直接从缓存进 bundle，**sha256 就是它的身份证**（与 bundle 清单天然对齐）。

---

## Solution

### Part A —— 离线 bundle

`tuoen bundle create <快照目录> --out <目录>` 产出一个**目录**（可选 `--archive` 打成 zip，走 bsdtar）：

```
<out>/
├─ bundle.toml                清单：schema_version / created_at / 快照的 content_hash / 逐项条目
├─ snapshot/                  tuoen.d/ 原样（八份文件）
├─ archives/<sha256>.zip      工具归档（从 L0 的缓存复制；文件名 = 内容 sha256）
└─ packages/
   ├─ npm/<name>-<version>.tgz
   └─ pip/<name>-<version>-<tags>.whl
```

**bundle 清单里每一项都必须说清它是"带字节的"还是"只有元数据 + 厂商 URL 的"**：

```toml
[[entry]]
kind = "tool-archive"          # tool-archive | npm-package | pip-package | snapshot
name = "node"
version = "24.19.0"
sha256 = "158f7685…"
size = 37618919
included = true                # false = 只有元数据 + url
url = "https://…/node-v24.19.0-win-x64.zip"   # included=false 时必需
licence = "redistributable"    # redistributable | url-only | unknown
licence_reason = "Node.js 的许可证允许再分发"   # 必须具体，不许写"许可问题"
```

- **许可门**（决策 32 的实现）：只有**允许再分发**的制品才带字节（Temurin / CPython / Node.js…）；
  Oracle JDK 8/11/17、MSVC 一律 `included = false` + `url-only`，并在还原时进 `manual_actions`
  （`licence-blocked`，`detail` 写具体原因 —— 与 L1 决策 152 同一条规矩）。
- **bundle 是"别人的数据"**：`restore --bundle` 之前**必须**逐项校验 sha256，
  任何一项不符就**拒绝使用**（不是警告）。bundle 内的相对路径不许逃出 bundle 根
  （与 L0 的归档条目名校验同一条红线）。
- **bundle 里不许有凭据材料**：`bundle create` 对 `snapshot/` 的每个文本文件跑 L1 的
  `secrets::scan_content`，命中就**拒绝创建**并说出是哪个文件（防御纵深：快照本来就不该有）。
- **`tuoen bundle verify <目录>`**：逐项校验哈希与大小，报告缺项/多项/损坏项，退出码 0/1。
- **`tuoen restore <快照> --bundle <目录>`**：所有制品从 bundle 取；
  **bundle 里没有的项一律报 `needs-network`，绝不偷偷上网**（"离线"必须是真的离线）。

### Part B —— GUI（Tauri v2）

新增 `crates/gui`（`src-tauri/` 是 workspace 成员，决策 30），**与 CLI 共用 `core`**，
不复制任何 schema。

**一条不可协商的规矩：GUI 不许做 CLI 做不到的事。** 每个动作都走同一套
`plan → 展示 → 确认 → apply`；GUI 的"计划"与 CLI `--json` 的计划在同一个固定装置上**逐字节相同**
（有测试钉住）。

信息架构（DESIGN §5 里"待 L3"的那一项，本 spec 定案）：

| 页面 | 内容 | 能不能写 |
|---|---|---|
| **总览** | 当前状态摘要（工具 / PATH / 环境变量 / 全局包 / 体检发现数）+ 路径（store / cache / shims / globals 根） | 只读 |
| **捕获** | 选 section → 预览 → 写 `tuoen.d/`；跳过清单单独一栏 | 写文件（用户选的目录） |
| **体检** | 发现列表，按严重度/id 过滤，每条显示 `evidence` 与 `message` | 只读（**没有"一键修复"**） |
| **还原** | 选快照 → **计划视图**（section 卡片 + 两个口径的计数 + `actions` + `manual_actions`）→ 确认 → 进度 → 逐节结果 | 写（确认后） |
| **全局包** | L2 的两个来源（`machine` / `tuoen`）+ 版本 + bin 名 | 只读 |
| **关于** | 版本、许可证（MIT OR Apache-2.0）、仓库地址、诊断包导出 | 只读 |

硬性约束：

- **GUI 永不提权**：机器级动作一律显示成 `manual_actions`（决策 12）。
- **没有计划就没有写入**：任何写操作都必须先看到计划并显式确认（决策 150 的 GUI 版）。
- **长任务可取消**：捕获/还原/体检在后台线程跑，界面有进度与取消；
  取消要**安全收场**（遵守"会启动进程的代码：三条铁律"，进程数有上限、有超时、跑完清场核对）。
- **资源全在本地**：不引用任何 CDN（目标人群的网络环境就是理由）；
  WebView2 缺失时给出明确的手工安装指引（进 `manual_actions`，不是崩溃）。
- **GUI 崩溃不许影响状态**：写入仍然在 `core` 的 staging + 原子翻转里，
  GUI 进程被杀不会留下半成品（与 CLI 同一条保证）。

---

## User Stories

### 离线机器

> 我在有网的机器上 `tuoen bundle create tuoen.d --out D:\bundle`，
> 它告诉我：8 份快照、3 个工具归档（135.9 MB）、7 个 npm 包、3 个 pip 包带字节；
> **2 个工具只有 URL**（Oracle JDK 与 MSVC 不允许再分发），并写清了原因。
> 我把 D:\bundle 拷到内网机器，`tuoen bundle verify` 全绿，
> `tuoen restore tuoen.d --bundle D:\bundle --apply` 装完了所有能装的，
> 剩下的两条在 `manual_actions` 里告诉我该手工做什么。

### 我拿到一个来路不明的 bundle

> 同事给了我一个 bundle。`tuoen bundle verify` 报第 4 项哈希不符 ——
> `restore` **拒绝使用它**，并告诉我哪一项、期望什么、实际什么。我不会拿它写我的系统。

### 我想看清楚再动手

> 我在 GUI 里选了快照，看到计划：`path` 会改 24 条、`env` 会写 1 条、
> `globals` 会装 7 个包（其中 1 个版本冲突，标出来了）、3 条人工待办。
> 我点了确认，进度条走完，每一节的结果都在页面上。

### 体检发现问题

> GUI 列出 29 条发现，我按 `error` 过滤，看到 `path.duplicate` 的 12 组证据。
> 我点"复制证据"，粘到 issue 里。**没有一键修复按钮** —— 这是故意的。

---

## Implementation Decisions

### bundle 是数据，不是代码（**本 spec 的新决定**）

`restore --bundle` 读取的一切（清单、快照、归档、包）都是**不可信输入**。
所以：先校验、再解析、再使用；路径一律相对且不许逃出根；
清单里的 `sha256` 是**唯一**的身份依据（不靠文件名、不靠大小）；
校验失败**拒绝使用**（不是"警告后继续"）。
这条与 L0 的归档条目名校验同源：**"名字看起来对"不是证据**。

### 工具归档直接复用 L0 的缓存（**本 spec 的新决定**）

L0 的缓存是 `sha256\<前两位>\<完整 sha256>` + `.meta.json`，本身内容寻址。
bundle 从缓存**复制**（不重新下载）；`archives/` 下的文件名就是 sha256 ⇒
bundle 清单与缓存**同一套身份**，校验只需要一次哈希。
缓存里没有的项：`included = false`（若许可允许且用户要求，bundle create 时才下载）。

### "bundle create 需要网络"必须说出来（**本 spec 的新决定**）

实测：`npm pack --offline` 即使包已安装也失败（`ENOTCACHED`）。
所以 `bundle create` 对 npm/pip 包**需要网络**（除非显式要求"只打包已有的"）。
产品的话必须说清楚：**bundle 在有网的机器上创建、在离线的机器上消费**。
`--only-cached` 开关允许"只打包本地已有的"，代价是清单里会出现 `included = false` 的项 ——
**宁可少打包，不许假装打全了**。

### GUI 的测试边界（**本 spec 的新决定**）

GUI 的**命令层**（Tauri command 函数）是薄适配器，输入输出都是 `core` 的类型 ⇒ 用普通单测测。
像素层不做自动化测试（诚实写在验收文档的"未覆盖"里），只做人工检查表。
**计划一致性测试**（GUI 的计划 == CLI 的 `--json` 计划，逐字节）是这一层唯一的"重"测试，
也是防止 GUI 长成第二套实现的唯一可靠办法。

---

## Testing Decisions

### 硬性约束

1. **测试不访问网络**（bundle 的创建/消费都用固定装置里的假归档与假 tarball）。
2. **测试不写真实系统状态**：`restore --bundle` 的测试用 `%TEMP%` 下的根 + `IsolatedHome`。
3. **bundle 的恶意输入必须有反例用例**：哈希不符、路径逃逸（`..\..\`）、清单里出现
   绝对路径、条目缺失、条目多余、清单自身损坏 —— 各一条，且**必须红**。
4. **许可门必须有反例用例**：一个 `url-only` 的制品**不许**被写进 `archives/`。
5. **GUI 的计划一致性测试必须有**：同一个固定装置，GUI 计划与 CLI `--json` 逐字节相同。

### 必须有的具体用例

- `bundle create` 在"缓存里有 2 份归档、1 个包需要下载"的固定装置上：清单里
  `included` 的分布正确，且**创建过程没有访问网络**（当 `--only-cached`）。
- `bundle verify`：全绿 / 一项哈希不符 / 一项缺失 / 一项多余，四种各一条。
- `restore --bundle`：bundle 里缺一个包 → 那一项 `needs-network`，**其余照装**，
  退出码与人类输出都说清"7 个里成了 6 个，1 个需要网络"。
- 归档条目名：`..\evil`、`CON`、`trailing.`、大小写碰撞 —— 沿用 L0 的用例（不重复实现，复用）。
- GUI：计划一致性；命令层的每个 command 一条单测（输入 → `core` 调用 → 输出）。

### 本 spec 不要求做的事

- 不要求 bundle 增量/差分（每次全量；差分是 L4 以后的事）。
- 不要求 GUI 的主题/无障碍（V1 只要求能读、能点、能取消）。
- 不要求跨平台（V1 只发 Windows）。
- 不要求自动更新（V1 手动下载）。

---

## Out of Scope

- **不托管任何二进制**（决策 34）：bundle 是**用户自己**在自己机器之间搬的东西，
  `tuoen` 不提供任何下载服务。
- **不绕过许可**：`url-only` 的制品在任何路径下都不会被塞进 bundle（有反例用例）。
- **不在 GUI 里提权**、**不做"一键修复"**、**不做后台常驻服务**。
- **不做 WebView2 的静默安装**（缺失时给指引）。
- **不做遥测**。

---

## Further Notes

### 为什么 L3 的危险点与前三层不同

L0/L1/L2 的输入都是"这台机器"或"我们的快照"。L3 的输入是**一个从别处拿来的目录**，
而它的输出是**对系统的写入**。这就是"解压炸弹 / zip-slip / 供应链"那一类问题的入口。
所以本层的判据只有一条：**先证明它是什么，再决定用它做什么** ——
哈希是身份，清单是契约，校验失败就是拒绝。

### 与 `mise` 的关系

`mise` 没有离线 bundle 这一层（它假设有网），GUI 也不是它的强项。
所以 L3 没有直接参照物；能借的是它的**资源抽象**思路（把"下载"与"安装"分开），
以及它在 Windows 上对 `WebView2` / `shim` 的既有经验。
