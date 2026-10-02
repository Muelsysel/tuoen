# tuoen · 拓境 — 设计规格

> **拓境 — 让你的 Windows 开发环境可搬运、可复现。**
> *Tuoen — capture, carry, and rebuild your Windows dev environment.*

本文件是**已达成共识的设计决策的固化**。每一条都来自与项目所有者的逐轮评审（`grilling` 工作流），
并尽量以本机取证或上游文档作为依据。**凡未经确认的推断都显式标注。**

---

## 0. 一句话定位

**整机开发状态（dev-state）的捕获与还原器**，而非又一个包安装器。

理由：按名字安装包、项目级 pin、shim 机制已被商品化（调研记录 6 个活跃 + 2 个已死的多语言版本管理器，
其中 `mise` 为 34.5k★ 的 Rust 项目）。而**"整机 dev-state 的捕获/还原"无人占据**——
`winget export` 只导包 ID 并对无法匹配项给警告；`scoop export` 只导应用；
`chezmoi` 管文件不管机器；`winget` 官方明确放弃了排序（winget#4940）。

**差异化排序**（据调研，仅前四项进入 V1 范围）：

1. 机器可读的整机 dev-state 模型，双向，`plan → diff → apply`
2. 提权作为一等资源来规划（逐项 scope、一次同意、明确的非管理员降级路径）
3. PATH 作为受管资源（所有者、顺序、长度预算、重复与死条目检测、声明式移除）
4. 采纳既有安装而不是重装

**中文优先**与上述定位互补：最痛的人群正是国内网络环境 + 需在内网/离线机器上重建开发环境的人，
而 `mise` 的 `bootstrap` 系统资源是 Linux/macOS 形状的。

---

## 1. 决策日志

### 1.1 定位与范围

| # | 决策 | 结论 | 关键理由 |
|---|---|---|---|
| 1 | 工具身份 | **完整包管理器**，自建 manifest 体系与安装引擎；winget 仅作可能的 manifest 来源，**不是执行后端** | 走"编排器"路线则"换电脑"的还原保真度上限 = winget 能否复现精确版本，无法承诺 lock 级重建 |
| 2 | 技术栈 | **Rust** | 决定性理由是分发与自举：管理 Node/Python 的工具若要求先装 Node 是逻辑循环；产出单个 `.exe` |
| 3 | 与 winget/Scoop 的边界 | **检测但隔离**：只读检测，**绝不改写它们的 PATH 与状态** | 改写会制造"两个包管理器互相破坏"的 bug，而这类 bug 会被算到我们头上 |
| 4 | 招牌功能 | **整机 dev-state 的捕获与还原** | 见 §0 |
| 5 | 平台范围 | **Windows 优先，在 I/O 与平台 API 处留清晰 seam**；V1 只发 Windows 二进制 | 留 seam 本身是好模块设计，成本极低；**不等于** V1 跨平台 |
| 6 | 默认语言 | **中文优先**：默认中文界面 + 内置国内镜像 + 中文文档优先 | 见 §0 |
| 7 | 落地顺序 | L0 地基 → L1 可用 → L2 扩展 → L3 重资产；**镜像加速提前进 L0** | 镜像加速不是"功能"而是**下载层的属性**，事后往写死的下载器里塞源优选代价高得多 |
| 8 | 名称 | **`tuoen` / 拓境**；tagline：**拓境 — 让你的 Windows 开发环境可搬运、可复现** | 造词，不表意，故 tagline 承担全部定位信息 |

### 1.2 功能清单

V1 全做，分四层：

- **L0 地基** — manifest schema + 解析 + 下载/校验/解压 + 原子安装 + junction 翻转 + `install`/`list`/`uninstall`；**镜像加速（源优选）内建在下载层**
- **L1 可用** — 多版本共存 + 项目级 pin（`shell`/`trust`）+ **整机状态捕获与还原** + 体检诊断
- **L2 扩展** — 全局包管理（npm -g / pip）
- **L3 重资产** — 离线 bundle + GUI

### 1.3 安装与版本机制

| # | 决策 | 结论 | 关键理由 |
|---|---|---|---|
| 9 | 版本切换 | **Junction 优先，symlink 作为可选** | 本机实测：Developer Mode 关闭 + 未提权时 `New-Item -ItemType SymbolicLink` 文件与目录**都失败**，而 `New-Item -ItemType Junction` **成功**；微软文档明确 `SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE`(0x2) **只在 Developer Mode 已开启时有效**，传 flag 不是绕路方案 |
| 10 | 命令暴露 | **自建真 `.exe` shim**，**只发 `.exe`，不发 `.cmd`/`.ps1`** | ① 用户级工具在 PATH 上永远排在机器级条目之后（实测：进程 PATH 首条为机器条目），只能靠 shim 抢名字；② `.cmd` shim 在 Node ≥18.20.2/20.12.2/21.7.3 之后**无法被 spawn**（CVE-2024-27980，nodejs/node#52681） |
| 11 | shim 版本解析 | **生成时写死目标路径** | 因为用 junction 翻转，shim 指向稳定的 `.../current/<tool>.exe`，**切版本根本不碰 shim**，只改 junction 目标。省掉每次调用的配置读取与解析（实测原生 shim 33–36ms vs C# 86ms，差距正在此处） |
| 12 | 提权策略 | **默认用户级；提供可选提权的机器级安装** | 检测到机器级条目遮蔽我们的 shim 时，明确告知"这 N 个条目遮蔽了我们"，用户可选择提权接管。**绝不静默改写系统状态**（与决策 3 一致） |
| 13 | 第三方安装 | **只读发现，不接管** | 能看见、能选中、能 pin、能使用；但 `uninstall` 不会去动它们。理由：接管 nvm4w 意味着改写它那个**需要管理员才能重建的符号链接**，且两个版本管理器会争抢同一个 reparse point |

### 1.4 项目级 pin

| # | 决策 | 结论 | 关键理由 |
|---|---|---|---|
| 14 | 声明方式 | **单文件 `tuoen.toml`** | 管的是**一整条互相约束的工具链**（JDK+Node+Python 版本相互约束），分开声明会漂移 |
| 15 | 生效方式 | `tuoen shell` 永远可用；**自动切换需 `tuoen trust` 一次** | 进入陌生仓库就自动改工具链版本是安全漏洞（恶意仓库可 pin 一个带后门的"Node 版本"） |
| 16 | 信任记录 | **用户级中央清单**（`%APPDATA%\tuoen\trust.toml`），记录绝对路径 + **首次信任时的指纹** | 信任标记若放在被信任的目录内，任何 clone 下来就自带信任标记，等于没有机制；指纹是因为路径可能被替换（删目录再 clone 同名目录） |
| 17 | 企业策略 | 信任清单**格式设计成未来可被系统级策略覆盖** | 个人用户不需要提权，但企业需要统一管控 |

### 1.5 数据与状态模型

| # | 决策 | 结论 | 关键理由 |
|---|---|---|---|
| 18 | manifest schema | **两层：catalog（工具元数据）+ recipe（版本安装步骤）** | 许可证字段必须挂在"工具"层，但 Oracle 的规则**按版本分界**（JDK 21+ 为 NFTC 可镜像；JDK 8/11/17 不可再分发），两层的切分点正好落在"版本"上 |
| 19 | 包目录来源 | **内置种子目录 + 可选远程签名索引** | 全内置则每次上游发版都要跟着发版；全远程则无网络用户什么都装不了（与"离线包"自相矛盾）。先例：Python 3.14 官方安装器即 `index.json` + Authenticode 签名目录 + `requires_signature` |
| 20 | 变更引擎 | **统一 plan/diff/apply**，所有变更都先产出 plan，`--dry-run` 走同一套代码路径 | 若只有 PATH 能预览，则最大的一次变更（还原到新机器）反而没有预览；且两套代码路径必然漂移，**漂移的预览比没有预览更危险** |
| 21 | 状态边界 | 捕获：我们管理的安装 + 检测到的第三方安装 + 环境变量 + PATH 结构 + 配置文件 + WSL/.vsconfig + 全局包清单，**逐项标注来源与置信度**；排除：缓存、**密钥材料**、项目代码、系统组件、进程噪声 | 只捕获自己装的 → 在作者本机上这个功能几乎是空的（node 是 nvm4w 装的、Python 是官方装的、JDK 是手工解压的） |
| 22 | 状态文件 | **目录 `tuoen.d/` 分文件**（tools / path / env / configs / wsl …） | "PATH 变了"与"Java 版本变了"是两个独立 diff；也让"只应用 PATH 部分"这种选择性操作在格式层面天然成立 |
| 23 | 密钥策略 | **主动扫描 + 排除 + 报告跳过清单** | 仅靠敏感路径黑名单会漏（作者本机的 `.m2/settings.xml` 不在任何黑名单里却藏着活 PAT）。原则：**捕获"意图清单"（版本/路径/名字），绝不捕获"材料"（密钥/token/密文）** |
| 24 | 幽灵条目 | **只报告，破坏性动作留给用户显式触发** | "自动清理注册表和 PATH"出错即无法挽回，而检测本身也可能误判（如把网络驱动器上的真实安装当幽灵） |
| 25 | 检测信任层级 | **分层报告 + 标注来源与置信度** | 把"PATH 上确认可执行"与"注册表声称已装但文件缺失"并列展示，会让用户看一眼就不再信任这个工具 |
| 26 | 全局包管理 | **捕获还原 + 按 Node 版本隔离** | 本机陷阱：全局 npm 包装进了 `C:\nvm4w\nodejs` 符号链接内部，**切 Node 版本会静默"丢失"它们**；只做捕获还原则清单会与实际状态脱节而用户不会察觉 |
| 27 | 全局包前缀 | **环境变量重定向**（`NPM_CONFIG_PREFIX` 及 pip 等价物），不写用户配置文件 | 写 `.npmrc` 会覆盖用户自己的配置；且 pnpm 曾因仓库本地 `.npmrc` 展开 `${ENV}` 导致密钥外泄而不得不停止该行为 |
| 28 | PATH 重建 | **重建 + diff 预览 + 选择性应用** | 本机 PATH 已是坏的（10 组重复、7 条失效、1 个重复 3 次的拼写错误、11 条硬编码用户名）。"原样搬运"会把旧病一起移植 |

### 1.6 工程与发布

| # | 决策 | 结论 | 关键理由 |
|---|---|---|---|
| 29 | 仓库结构 | **Cargo workspace 多 crate**（`core` / `platform` / `manifest` / `cli` / `gui` / `shim`） | shim 是每次敲 `node` 都要跑的程序，必须独立成极小二进制，不能把 GUI 的依赖图拖进启动路径；跨平台 seam 需要独立 `platform` crate 才不被业务逻辑渗透 |
| 30 | GUI 框架 | **Tauri v2** | Tauri 的 GUI 进程本身是 Rust 二进制，官方文档允许 `src-tauri/` 作为 **Rust workspace 成员** → CLI 与 GUI 共用同一套类型，schema 一改两边一起报错。Electron 则要求为每个 Rust struct 永久维护 TypeScript 镜像 |
| 31 | 许可证 | **MIT OR Apache-2.0 双许可** | Rust 生态事实标准；同时给出 MIT 的简洁与 Apache 的显式专利授权 |
| 32 | 离线 bundle | **元数据 + 厂商 URL + 哈希清单**；自包含归档**只装许可允许再分发的**（Temurin / CPython / Node.js） | Oracle JDK 8/11/17 不可再分发、MSVC 完全不可再分发；"再分发别人的安装器"是这类项目最容易踩的坑 |
| 33 | 命名一致性 | `tuoen.toml` / `tuoen.lock` / `tuoen.d/` / `%APPDATA%\tuoen` / 环境变量前缀 `TUOEN_` | 一致性 |
| 34 | 镜像加速边界 | **源优选 + 用户自定义镜像模板**，**不托管任何二进制** | 不托管二进制意味着不承担再分发责任，这是自建引擎能安全落地的前提；自定义模板同时覆盖"企业内网"与"离线包"两类需求 |
| 35 | 输出稳定性 | **`--json` 输出必须稳定、不本地化** | 否则脚本化与未来的 GUI 会被中文界面绑死 |

### 1.7 L0/L1 实现期新增（决策 36–45）

这十条是**实现过程中被真机输出逼出来**的，不是设计阶段想到的。其中 36–38 已经过项目所有者拍板。

| # | 决策 | 结论 | 关键理由 |
|---|---|---|---|
| 36 | 检测的置信度层级 | **七层**：`managed` / `executable` / `registered` / `manager-owned` / `directory-only` / `registered-missing` / `alias-ghost` | **`registered` 是必需的第七层**：ARP 的 `InstallLocation` 常常是一个**目录**（本机 `C:\Program Files\Git\`、`C:\Program Files\Java\jre1.8.0_491\`、`C:\Program Files\WSL Dashboard\`），且它**不在 `PATH` 上**。把它报成 `executable` 违反 `executable` 自己的判据（"在 `PATH` 上能解析到、文件真实存在且大小 > 0" —— 三条一条都不满足）。它与 `directory-only` 的区别是**有注册表记录**（因此可重建），与 `registered-missing` 的区别是**目录真的在**（所以不是幽灵） |
| 37 | 报告粒度 | **每个工具一行，只报主命令** | 按命令报会让 `node` 变成 4 行、四个不同版本号并列（`node.exe 24.19.0` / `npm.cmd 11.17.0` / `npx.cmd 11.17.0` / `corepack.cmd 0.35.0`），用户没法回答"我的 Node 是哪个版本"。次要命令（`javac` / `npm` / `pip` / `uvx`）**仍然被发现**（shim 需要知道它们在哪）并参与遮蔽判定，只是不单独占行 |
| 38 | 持久环境变量的注册表路径 | 用户级是 `HKCU\Environment`，机器级是 `HKLM\SYSTEM\CurrentControlSet\Control\Session Manager\Environment` —— **两者都不在 `SOFTWARE` 下面** | 这条是**用一条真 bug 换来的**：`RegHive::Hkcu` 会给不带绝对前缀的子键自动拼 `SOFTWARE\`，于是"读用户级环境变量"实际去读 `HKCU\SOFTWARE\Environment`（不存在），**静默返回空表**。症状是"不报错、只是少了一半数据"，比报错危险得多 —— 见 `docs/acceptance/L1-01-detect.md` §6.1 |
| 39 | HTTP 传输 | **shell out 到系统 `curl.exe`**（Schannel 后端），**不打包 TLS 栈** | ①Windows 10 1803+ 自带 `curl 8.21.0`（Schannel + zlib + HTTP/2 + `--location` / `--connect-timeout` / `--fail-with-body` / `--proxy` 全都有）；②项目已经信任系统 `tar.exe` 做解压，同一条理由；③**Schannel 直接用 Windows 证书存储 → 企业 TLS 拦截的根证书自动被信任**，而打包 rustls + webpki-roots 在内网里必然连不上；④避免把 ~40 个 crate 的依赖图引进一个必需品。传输层在 `Transport` trait 后面，将来换纯 Rust 实现不影响上层 |
| 40 | 镜像选源 | **内置顺序只是默认值，真实的选源靠探测**：`tuoen fetch --probe-sources` 对候选源发 range GET 实测，把可达性与耗时排序并缓存 | **实测逼出来的**：本机对这个网络探测，`mirrors.tuna.tsinghua.edu.cn` 与 `mirrors.ustc.edu.cn` **全部不可用**，`mirrors.aliyun.com/adoptium` **404**（目录结构不同），而 `cdn.npmmirror.com` 94ms、`mirrors.cloud.tencent.com` 267ms、`nodejs.org` 280ms、`api.adoptium.net` 327ms 都可用。"抄一份国内镜像 URL 清单"的做法在这里会直接失效 —— 清单本身不是可用性。**探测目标有三条硬要求**（每条都是真机换来的）：①**必须是文件不能是目录** —— 不认区间请求的服务器会把 `-r 0-0` 当普通 GET，目标是目录时本机实测下了 **155794 字节**的版本列表（"为了问一句能不能到"）；②**不能带版本号** —— 镜像会剪枝旧版本，那时探测会把一个能用的源判成不可用；③**必须与镜像前缀分开** —— 否则阿里源会被自己的探测淘汰（它的制品前缀目录 404 而制品完全正常）。三条合起来给出的目标是 `<mirror_prefix>index.json`。**判决稳定但状态码不稳定**：同一个 URL 跨运行出现过 403 / 200+整份列举 / 404，所以分类只能到 `source-unavailable` 一层，不能基于状态码写"这个镜像坏了"的判断 |
| 41 | SHA-256 | **自实现，不加 `sha2` crate** | 理由不是"省一个依赖"，而是**这段代码的位置**：它站在"我们信任这个字节流"与"我们把它解压到磁盘"之间，是整个工具里最该被独立核对的一段。它是 FIPS 180-4 的固定算法（无版本演进、无平台差异、无配置项），纯函数、约 120 行、零依赖零 IO，**且有官方测试向量** —— "对不对"不靠信任，靠跑。**代价是真实的**：本区间在这段代码上修了四个独立缺陷（`finish` 的填充公式在 `total + 1 > 56` 时 `u64` 下溢；`finish` 从来没压缩最终块 —— "总长是 64 的整数倍"不等于"缓冲是空的"；`update` 的尾巴被替换而不是追加，所以**逐字节喂与一次喂结果不同**；`update` 里 `bytes` 没重新切片，多用原始长度算了一次块数）。**教训**：我曾在这里放了一条 `debug_assert_eq!(self.buffered, 0)`，它让一个**正确**的实现看起来是坏的，排查代价是一整轮 —— **错的是断言，不是代码**；一条错误的断言比没有断言更贵 |
| 42 | 什么时候换源 | **只有源级失败才换源**（`source-unavailable`：连不上 / 超时 / 4xx / 5xx / TLS 失败）；**哈希不符立刻停**（`should_try_next_source()` 对 `Integrity` 返回 false） | 被污染的字节不该靠"换个镜像"来修好 —— 换源会让"哈希不符"变成"重试了几次之后成功了"，而那句成功是假的。所以哈希不符直接报 `ChecksumMismatch`（同时给出期望值与实际值），不报 `AllSourcesFailed`（那意味着"每个源都试过了"）。**副产品**：分类是在真机上判的，而本机的 DNS 对所有域名返回伪 IP（`198.18.0.0/15`，RFC 2544 段）→ "域名不存在"在这里表现为 **TLS 握手失败（curl 退出码 35）而不是 DNS 失败（退出码 6）**，所以分类逻辑不能建在"退出码 6 = 域名解析失败"上 |
| 43 | 探测/下载的传输层看什么 | **先看 `-w` 给出的 HTTP 状态码，再看 curl 退出码**；`FetchOutcome` 必须带回**真实**状态码（不许写死 `206`） | 两个真 bug 换来的：①`--fail-with-body` 让所有 4xx/5xx 的退出码都是 **22**，先判退出码会让 `DownloadError::Http` 那条分支**永远走不到**（所有镜像 404 都报成"curl 退出码 22"）；而"连不上"时 `-w` 只给 `000`，所以**两者都要，只是先后不同**。②探测把状态码写死 `206` 之后，"这个源不认区间请求、正在下整包"这件事完全看不见 —— 而它真的发生了（见决策 40 的 ①）。**一条中国的镜像站可以合法地拒绝一切非浏览器客户端**：清华的 403 响应体原文是「您访问使用的软件带有非常用软件的特征」，所以"403"不等于"镜像缺失" |
| 44 | 归档解压 | **三层防线，一层都不能省**：①解压前我们自己逐条目校验名字（`crates/archive/src/name.rs`）；②解压交给系统 `tar.exe`，**并要求退出码为 0**；③解压后**审计落盘结果**（`crates/archive/src/audit.rs`） | 三层不是"防御纵深"这种好听的话，是**三个实测出来的洞**：**①必须自己校验**，因为 bsdtar 对路径逃逸可靠（`..` / symlink / 硬链接 / fifo / 设备**全拒**，而且跳过任何东西退出码就是 1）**但对名字的危险形态完全不可靠** —— `CON` 与 `sub/NUL.txt` 真的会被创建（用 `\\?\` 绕过 Win32），而创建出来的文件**普通路径看不见**（`Test-Path` 报 False、`Test-Path -LiteralPath '\\?\…\NUL'` 报 True）→ 清不掉的残骸；`trailing.` / `trailing ` 真的会被创建而 Win32 会吃掉结尾的点与空格 → 打不开删不掉；`stream.txt:evil` 被**静默改名**成 `stream.txt_evil`；`Readme.txt` + `README.TXT` 只剩一个（NTFS 大小写不敏感 → **静默覆盖**，归档可以借此偷偷替换自己的合法文件）。**②必须要求退出码 0**，因为非零就意味着有条目被拒 = 磁盘上是**半个目录**。**③必须审计磁盘**，因为 `-tf` 的列表是**有损的**：bsdtar 把控制字符转义成 `\n`/`\r`，而**转义是双向的** —— 真目录 `bin` + 文件 `node.exe` 与"名字里含换行的 `bin<LF>ode.exe`"在列表里**长得一模一样**；`-tf` 也**看不出条目类型**（symlink 在它眼里就是一行字）。**列表是意图，磁盘是事实。** 附带一条推论：`.7z` **不是缺口** —— libarchive 3.8.8 自己就认它（真 7z 实测 2454 条目、解出的 `node.exe` 报 `v24.19.0`），原设计假设被推翻 |
| 45 | 报哪一种名字违规 | **`-tf` 里一个 `\n` 有两种解释，按"哪种更可能是真的"报**：分隔符解释**通过**就接受它；不通过且名字里有转义序列就报 `ControlCharacter`；不通过且没有转义序列就报分隔符解释给出的那条 | 这条是**被一个测试逼出来的**：`evil\n../escaped.txt` 原本报成 `TrailingDotOrSpace`（因为 `evil/n../escaped.txt` 里有个段 `n..` 以点结尾）—— 判决对了但**理由完全错**，而票据明确要求"给出具体原因，因为'解压失败'不足以判断是上游问题还是攻击"。**关键是不能反过来做**：把 `\n` 一律当控制字符会**拒绝掉全部 7z 制品**（7z 的条目名就是用反斜杠的：实测 npmmirror 那份列出来是 `node-v24.19.0-win-x64\node.exe`）。两条边界也验过：`\\` **不算**转义（否则 `\\server\share\x` 会报成 ControlCharacter 而不是 UncPath），`\7zip` **不算**转义（三位八进制 `\ooo` 才是，否则正经路径被误拒） |

---

## 2. 平台硬约束（来自本机实测，非推断）

这些是**必须绕着走的地面事实**，实现时不得假设相反情况。

### 2.1 PATH

```
进程 PATH 顺序 = [进程注入项] ; HKLM Path ; HKCU Path
                    ↑ 实测首条为机器条目 → 用户级工具只能追加到尾部
                    → 在名字冲突上永远输给机器级条目 → 必须用 shim
```
- **机器 Path 与用户 Path 都实测不含 `%VAR%`**（HKLM 标了 `REG_EXPAND_SZ` 却零变量）
- **写 PATH 绝不用 `setx`**：文档写 1024 字符上限且**内容是"被裁剪后应用"**（静默数据丢失），且操作已存在的值会**永久展开所有 `%VAR%` 引用**。本机算术：用户 725 + 机器 978 = 1704 字符，`setx PATH "%PATH%;..."` 会**既裁剪又把整条机器 PATH 永久复制进 HKCU**
- **正确做法**（Scoop 源码同款）：直接写 `HKCU\Environment`，**值含 `%` 才用 `REG_EXPAND_SZ`，否则保留原类型，再否则 `REG_SZ`**；读取用 `DoNotExpandEnvironmentNames`；随后广播
  `SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE=0x1a, 0, L"Environment", SMTO_ABORTIFHUNG=2, 5000, &out)`
- **两个不同的悬崖**：`setx` 截断 1024 字符；**`cmd.exe` 在 8191 字符后完全忽略 `Path`**——超过后**整条 PATH 一次性全部失效**，所有命令都报"不是内部或外部命令"。必须做**长度预算告警**
- **变更可见性**：环境块在 `CreateProcess` 时复制，**只有 Explorer 及其之后新起的子进程**能拿到；已在运行的 cmd/PowerShell/VS Code/IDE/服务/计划任务都拿不到，我们自己的进程也拿不到（除非显式打补丁）。"请重启终端"是平台限制，不是工具不友好
- **换机失效**：本机 11 条 PATH 硬编码了用户名（其中 **2 条在机器级**）。换账号名后静默失效，而 `Test-Path` 在本机仍通过 → **朴素校验器抓不到，必须显式检测用户名依赖**

### 2.2 文件系统与 reparse point

- **symlink**：Developer Mode 关闭 + 未提权时**文件与目录都创建失败**；`0x2` flag 无效
- **Junction**：免管理员可用；**但目标名可能嵌安装序号**（本机 Oracle 的 `java8path_target_1783390`），不可预测，必须记录目标而非猜测
- **硬链接**：仅 NTFS、仅文件、必须同卷；且当应用用 rename 替换自己的文件时会断（aqua 因此选硬链接，需注意此限制）
- **App Execution Aliases**：`...\WindowsApps\{python.exe, python3.exe, winget.exe, wsl.exe}` 是 reparse tag `0x8000001b`、长度 0 的别名，**既不是文件也不是符号链接**——基于复制的捕获什么也拿不到，而 `Test-Path` 式检查会通过，`Get-Command python` **成功但返回 0 字节幽灵**
- **长路径**：需要 `LongPathsEnabled=1` **且** 程序清单含 `<ws2:longPathAware>true</ws2:longPathAware>`。**相对路径永远受 MAX_PATH 限制**（`\\?\` 无法加前缀，实测约 300 字符的相对路径失败）→ 自己的解压/重命名代码加 `\\?\` 前缀，**并把 shell 集成视为永久 260 字符受限**
- **PATH 上的空格**：本机 48 条中 14 条含空格（29%），最糟的是 `C:\Dev\IDE\IDEA26\IntelliJ IDEA 2026.1.1\bin`（**同时含空格和版本号**）→ 不加引号的 shim 设计会当场崩

### 2.3 归档格式

- **系统自带 `tar.exe` 是 bsdtar 3.8.8 / libarchive 3.8.8**（含 libzstd 1.5.7 + liblzma 5.8.1）→ `.zip` / `.tar.gz` / `.tar.xz` / `.tar.zst` / `.tar.bz2` 可靠 shellout 解决
- ~~**`.7z` 是唯一缺口**，需要内置库~~ —— **实测推翻了这一条（决策 44）**：
  libarchive 3.8.8 自己就认 7z。拿阿里镜像那份真的
  `node-v24.19.0-win-x64.7z`（23,441,591 字节，哈希与上游一致）实测：
  `tar.exe -tf` 列出 2454 个条目、`-xf` 解出的 `node.exe` 能跑并报 `v24.19.0`。
  **不需要内置任何 7z 库。**

### 2.4 区域与编码

- 本机 zh-CN，**PowerShell 的符号链接报错是本地化的**（`此操作需要管理员权限。`）→ **永远不要匹配错误文本，要匹配 HResult / Win32 码**
- `git --version` / `java -version` 输出到 **stderr**，`node -v` 到 **stdout** → 版本探测必须同时读两路

### 2.5 安全

- **Zip Slip 在这个威胁模型里仍然活跃**（CVE-2026-27800 Zed `extract_zip` 缺 `../` 校验；CVE-2026-28486 安装命令期间的路径穿越）。解压检查清单：先规范化再断言在根目录内、拒绝绝对路径/UNC/`..`、校验 **symlink 条目目标**、拒绝保留设备名（`CON`/`PRN`/`AUX`/`NUL`/`COM1-9`/`LPT1-9`）、拒绝结尾点和空格、拒绝名字里的 `:`（ADS 写原语）、大小写碰撞检测、大小/条目数上限
- **MSVC / VS Build Tools 不是包**：微软文档说 `--quiet` "对标准用户不可编程使用"、安装操作"需要管理员权限"、且**不可再分发** → **建模为带外管理员前置条件，唯一正确做法是以管理员调用微软自己的 bootstrapper + `.vsconfig`**

---

## 3. 许可与再分发（产品级约束，不是法务脚注）

**每个制品的许可证必须是模型里可见的一等字段**，并**拒绝一切"不得收费/禁止再分发"条款的东西**。这既是真差异化也是项目自保。

| 上游 | 结论 |
|---|---|
| winget-pkgs manifests | **MIT** — 安全（仅元数据：URL + `InstallerSha256`） |
| Scoop Main bucket | **Unlicense**（公有领域）— 安全 |
| winget `msstore` 源 | **受限** — 不可当作可再分发索引 |
| **Chocolatey 社区仓库 CCR** | ⚠ **2027-01-01 起禁止组织内直接使用，且明确覆盖第三方工具** → **绝不默认启用** |
| Oracle JDK 21+ (NFTC) | **有条件** — 可再分发未修改版本，**前提是不收费** |
| Oracle JDK 8/11/17 | **不可再分发** |
| Eclipse Temurin | **安全**（GPLv2+CE） |
| CPython | **安全**（PSF） |
| Node.js | **安全**（MIT；商标另受 OpenJS 管辖，别把再分发品牌化成 "Node.js"） |
| **MSVC / VS Build Tools** | **不可再分发** |

**三条结构原则**：① 默认**只镜像元数据，不镜像二进制**——指向厂商 URL + SHA256（两种 manifest 格式里哈希都已存在）；② 只在许可明确允许处（Temurin / CPython / Node.js）重新托管；③ 绝不默认启用 Chocolatey CCR。

---

## 4. 命名与文件布局

```
tuoen.toml              项目级 pin（进 git）
tuoen.lock              解析后的精确版本（进 git）
tuoen.d/                整机状态快照目录
  ├─ tools.toml         工具与版本（含来源与置信度）
  ├─ path.toml          PATH 结构（所有者、顺序、长度预算）
  ├─ env.toml           环境变量
  ├─ configs.toml       配置文件清单（含"跳过原因"）
  ├─ wsl.toml           WSL 发行版与路径
  └─ globals.toml       全局包清单（按工具版本隔离）
%APPDATA%\tuoen\
  ├─ trust.toml         已信任目录 + 指纹（格式预留系统级策略覆盖）
  ├─ config.toml        用户配置（镜像、语言）
  └─ cache/             下载缓存
TUOEN_*                 环境变量前缀
```

**tagline 固定伴随**：中文 `拓境 — 让你的 Windows 开发环境可搬运、可复现。`；英文 `Tuoen — capture, carry, and rebuild your Windows dev environment.`

---

## 5. 待定项

| 项 | 状态 |
|---|---|
| Windows 代码签名方案（中国大陆个人是否可用 Azure Trusted Signing / 各 CA 的 OV 证书） | **调研中** |
| `tuoen` 的注册表可用性终筛（crates.io / npm / PyPI / GitHub handle / 商标 / 域名） | **调研中** |
| catalog 的签名方案细节（是否采用 Authenticode `.cat` 目录钉扎，参照 Python 3.14 官方安装器） | 待 Round 7 |
| 状态捕获的字段级 schema | 待 Round 7 |
| GUI 的具体信息架构 | 待 L3 |

---

## 6. 参考实现

**遇到实现问题时的第一参考对象是 `mise`**（Rust、MIT、34.5k★、周更）——尤其是 Windows 上的 shim 处理、
PATH 管理、`bootstrap` 的资源抽象，以及其 issue 区对已知 Windows 坑的记录。
其他参考：Scoop（`link_current` 用 junction 并 `attrib +R /L`、`WM_SETTINGCHANGE` 签名、shim 的参数转发与退出码透传）、
aqua（为何选硬链接）、vfox（反例：用 symlink + PATH 优先级，在 Windows 上需要提权或 Developer Mode，**抄之前先测**）、
Python 3.14 官方安装器（签名目录模型）。

---

*本文件的每条决策均可追溯到一轮评审与一项证据。未经确认的推断均已显式标注。*
