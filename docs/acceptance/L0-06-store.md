# L0-06 验收：安装、切换、删除、列表端到端可用（票据 #6）

**结论：通过。** 真机验收脚本 `scripts/acceptance-L0-06.ps1` 退出码 **0**，
**59 条断言全部通过**，原始记录 449 行在
[`L0-06-store-machine.txt`](L0-06-store-machine.txt)。

票据 #6 的验收标准是"`install` / `use` / `uninstall` / `list` 端到端可用；
只翻转 junction，不重写 `PATH`、不重写 shim；翻转原子；多版本共存；
junction 失败有降级路径不崩；`uninstall` 删当前版本要警告"。
这份文档逐条给出**判据**与**证据**。

---

## §1 这一票把三段已有的东西接了起来

前三张票各自交付了一个可单独验证的部件，但**没有一张能单独回答"能装上吗"**：

| 票 | 交付 | 它能自己证明什么 | 它不能证明什么 |
|---|---|---|---|
| #4 | `tuoen-download` | 能从镜像下到一个字节并对上哈希 | 下到的东西是不是能跑 |
| #5 | `tuoen-archive` | 能把归档安全地铺到磁盘上 | 铺出来的东西放在哪、怎么用 |
| #6（本票） | `tuoen-store` | 装在哪、谁装的、现在生效哪个 | —— |

安装链路：

```
内置目录 ──resolve──▶ ResolvedRecipe ──gate──▶ 允许？
                                             │
                    download（源优选 + 内容寻址缓存 + SHA256 校验）
                                             │
                    archive（三层防线 + 同卷临时目录 + 一次 rename）
                                             │
                    store.adopt_payload（搬进存储 + 写记录）
                                             │
                    可选：store.activate（一次 IOCTL 翻转 current）
```

**哈希来自内置目录，不来自下载源。** 这是这一票里最容易被"顺手做对"而做错的一件事：
从你正在下载的那台服务器上取哈希等于没有校验 —— 能改制品的人也能改哈希。
`crates/download/examples/fetch_probe.rs`（票据 #4）确实会去读上游的
`SHASUMS256.txt`，但那是**取证**（证明目录里的哈希与上游一致），不是安装路径。

---

## §2 怎么验的：三层，各管各的问题

| 层 | 位置 | 数量 | 回答什么 |
|---|---|---|---|
| 单元测试 | 各 crate 的 `#[cfg(test)]` | 见 §7 | 纯函数与不变量 |
| **进程边界契约测试** | `crates/cli/tests/manage_contract.rs` | 23 | 用户敲下去会发生什么（参数、错误码、退出码、`--json` 形状、磁盘副作用） |
| **真机验收** | `scripts/acceptance-L0-06.ps1` | 59 条断言 | 真的装上、真的能跑、真的切得动 |

**进程边界是本仓库的主 seam**（`docs/specs/L0-install-engine.md` 的测试决定），
所以中间那一层是有意做厚的：它不联网，把版本目录**直接摆进隔离存储**
（用 `tuoen_store` 自己的 API），因此跑得快、随时能跑、离线也绿。

真机验收那一层相反：它真的下载 35 MB、真的解压 101 MB、真的执行 `node.exe`，
并且**真的写这台机器的** `%LOCALAPPDATA%\tuoen\store`。所以它不在 `cargo test` 里。

---

## §3 真机证据（摘录；全文见 `.txt`）

### §3.1 「装上了」不是"文件在磁盘上"，是"它能跑"

```
$ tuoen install node@24.19.0
  实际下载：https://cdn.npmmirror.com/binaries/node/v24.19.0/node-v24.19.0-win-x64.zip（37304352 字节，源 `cn-npmmirror`）
          来自缓存（没有产生网络流量）
  解压：归档内 2454 个条目 → 落盘 1989 个文件、106112876 字节

$ "…\store\node\current\node.exe" --version
  v24.19.0
$ "…\store\node\current\node.exe" -e "console.log(process.execPath)"
  C:\Users\Muelsyse\AppData\Local\tuoen\store\node\current\node.exe
$ "…\store\node\current\npm.cmd" --version
  11.17.0
```

三条断言各有各的判据，**不能互相替代**：

- `--version` 报 `v24.19.0` → 链接指向了**正确的版本目录**；
- `execPath` 落在 **`current` 那条路径下** → 进程真的是从链接启动的
  （如果它报的是 `…\versions\24.19.0\node.exe`，说明链接被解析成了目标，
  这本身不算错，但我们就失去了一条"链接真的被用到了"的证据）；
- `npm.cmd` 也能跑 → **不是只有主命令通**。`npm.cmd` 是 `.cmd`，
  而 `.cmd` 走的是另一条解析路径（`PATHEXT`）。这一条是给票 #7 的 shim 提前探路。

`24.21.0` 在同一台机器上同时装着：`37618919` 字节的归档、
落盘 `1994` 个文件、`106986507` 字节。**多版本共存**在这里是可见的数字，不是说法。

### §3.2 原子翻转：`created` → `replaced`

```
$ tuoen use node 24.19.0 --json
{"…","repoint":{"degradedReason":null,"outcome":"created"},"previous":null,"version":"24.19.0"}

$ tuoen use node 24.21.0 --json
{"…","repoint":{"degradedReason":null,"outcome":"replaced"},"previous":"24.19.0","version":"24.21.0"}

$ fsutil reparsepoint query …\store\node\current
  LinkType   : Junction
  Target     : C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.21.0
  Attributes : Directory, ReparsePoint
```

**`replaced` 就是原子性的证据，而且它是实测出来的，不是从文档推的。**
文档对"文件已经有重解析点时会怎样"写得含糊（只说了标签不同会报
`ERROR_REPARSE_TAG_MISMATCH`），所以先问了一个可执行的问题：同标签时
`FSCTL_SET_REPARSE_POINT` 是**替换数据**还是**失败**？答案是替换 ——
于是整个翻转是**一次 IOCTL**，没有"先删再建"的空窗，
`current` 这个路径在任何时刻都是可解析的（`docs/DESIGN.md` 决策 46）。

**一条被这次验收抓出来的、关于"怎么断言"的教训**：`created` 与 `replaced`
是**同一个命令在不同状态下的两种正确输出**。第一版验收脚本把 `use` 跑了两次
（一次看输出、一次解析 JSON），于是拿"第二次的 `replaced`"去断言
"第一次应当是 `created`"，报了 3 条**假失败**。逐字节稳定性也一样 ——
必须用**同一状态下的两次调用**去比（§4.1）。

### §3.3 删当前版本：默认拒绝，且**拒绝必须零副作用**

```
$ tuoen uninstall node 24.21.0 --json
{"ok":false,"error":{"code":"active-version","message":"…先 `tuoen use` 到别的版本，或者加 `--force`…"}}
  → 退出码 1

  载荷目录存在: True
  链接存在    : True
  $ "…\current\node.exe" --version
  v24.21.0                      ← 被拒之后一切照旧
```

"拒绝"这件事有两种做法：**报错然后什么都不做**，与**先做一半再报错**。
只有前者是能用的 —— 所以这一节断言的不只是退出码，还有载荷、链接、
以及"通过链接仍然能执行且版本没变"。

`--force` 的顺序是**先摘 `current`，再删载荷**：

```
$ tuoen uninstall node 24.21.0 --force
  载荷目录存在: False
  链接存在    : False   ← 必须是 False，否则 current 会指向一个不存在的目录
  另一个版本在: True    ← --force 没有连累别的版本
```

顺序不是随意的：反过来的话，如果删载荷失败，磁盘上会留下一个
**指向不存在目录的 `current`** —— 而之后每一个 shim 都会报一个与真实原因
无关的错误。留下"一个不再被激活的版本"（另一种失败顺序的后果）明显更轻。

### §3.4 拒绝路径：每一条都是"稳定的错误码 + 能读懂的中文"

| 场景 | 错误码 | 退出码 | 输出里必须有 |
|---|---|---|---|
| 重复安装同一个版本 | `already-installed` | 1 | 怎么重装（`tuoen uninstall`） |
| 不可再分发的工具 | `licence-prohibited` | 1 | **具体原因**，且**不得出现任何 URL** |
| 不认识的工具 | `unknown-tool` | 1 | 我们认识哪些（`node, temurin, …`） |
| `node@`（写了 `@` 没写版本） | `empty-version` | 1 | 去掉 `@` 或者补上版本 |
| 切一个没装过的版本 | `not-installed` | 1 | **真正装过**哪些版本 |
| 删一个没装过的版本 | `not-installed` | 1 | —— |

两条刻意的设计：

1. **`node@` 与 `node` 是两种输入，不是一种。** 前者是"你写了 `@` 但忘了版本"，
   后者是"给我最新的"。混成一种会让一次手误**静默装上一个最新版**。
2. **许可证拒绝必须在解析 recipe 之前。** Oracle JDK 是种子目录里故意留的反例，
   它**没有任何 recipe** —— 顺序错了就会报成"没有适用的 recipe"，
   那是把"我们不许可你装"说成了"我们不知道去哪装"。

### §3.5 `managed` 层真的接上了

```
$ tuoen detect --json
  总条目数       : 29
  managed 条目数 : 1
    node  24.19.0  managed  C:\Users\Muelsyse\AppData\Local\tuoen\store\node\versions\24.19.0
      evidence: 由 tuoen 自己安装（存在我们的安装记录）

  置信度分布：
    managed: 1        executable: 12     registered: 4      manager-owned: 2
    directory-only: 2 registered-missing: 6               alias-ghost: 2
```

七层置信度的第一层（票据 #11 建的，当时恒为空）现在有数据了。
`node` 在结果里出现**三次**（`executable` / `manager-owned` / `managed`）——
那是**真实的分裂**（PATH 上的 `node` 来自 nvm4w，而我们管的这份在存储里），
不是重复（决策 37）。

**适配器没放在 `tuoen-platform` 里。** 存储布局（`<tool>/versions/<version>`、
`<version>.json`、`current`）的知识属于 `tuoen-store`，而 `platform`
**不能**依赖它（会成环）。所以在 `platform` 里再实现一遍是"能编译的"，
但会让同一份布局同时存在于两个 crate —— 而它们的偏差只有在
"`detect` 的结果与 `list` 不一致"时才暴露，那是最难归因的一类 bug。
真实实现在 `crates/cli/src/managed.rs` 的 `StoreManagedStore`。

---

## §4 验收脚本自己出过的两个错（都保留在这份文档里）

验收脚本是交付物的一部分，它自己也会错。两次都说出来，因为**教训比结论值钱**。

### §4.1 假失败与真失败长得一样

第一次跑，§8 报了"**`managed` 层是空的**，说明适配器没接上"——
而事实上适配器是好的。原因是脚本把 `detect --json` 的载荷键读成了 `tools`
（实际是 **`tool`**，单数），于是永远解析出 0 条。

修法**不只是改键名**：现在脚本先检查键在不在，
键都不在就报"**脚本自己读错了键**"并列出实际的顶层键。
"一条假失败与一条真失败长得一样"是这类脚本最贵的缺陷 ——
它会让你去修一个没坏的东西，或者更糟：让你**接受**一个坏掉的东西。

### §4.2 一份不能失败的验收不是验收

第一版脚本无论发生什么都 `exit 0`。现在每一节的期望都走 `Check`，
`§11 结论` 列出全部未通过的断言，退出码按它决定。

顺带抓出了 §4.1 之外的两条**假失败**（同一类根因）：`use` 跑了两次
（`created` vs `replaced`）、以及把"两次 `use` 的输出必须逐字节相同"写成了断言。
两者都是**脚本对"有状态的命令"用了一次性的期望**。

---

## §5 实现期抓出的两个真 bug

| # | 现象 | 根因 | 修法 |
|---|---|---|---|
| 1 | `list --json` 里 `installedVersions` 是 `null` | `ToolRecord` **没有** `rename_all = "camelCase"`（既有四个键都是单个单词，所以从来没暴露过这件事），于是新字段序列化成了 `installed_versions` —— 一个 snake_case 键混进一套 camelCase 契约 | 显式 `#[serde(rename = "installedVersions")]` |
| 2 | `install node@0.0.1-nonexistent` 的错误消息不提平台 | `ResolveError` 只说"没有匹配约束的 recipe"，而用户不知道我们是在哪个平台上找的 —— "版本不存在"与"这个平台不支持"因此分不开 | CLI 把平台包进消息：`在 windows-x64 上找不出 node@0.0.1-nonexistent：…` |

两条都是**被测试抓出来的**，不是被阅读抓出来的。第 1 条尤其说明
"新增一个字段"并不是零风险动作：形状契约的**键名风格**不会自己保持一致。

---

## §6 存储布局（这一票定下的契约）

```
%LOCALAPPDATA%\tuoen\store\
  .incoming\<tool>\<version>\        ← 解压临时区（与 versions/ 同卷 → 之后一次 rename）
  <tool>\
    versions\
      <version>\                     ← 载荷，与解压结果逐字节一致
      <version>.json                 ← 安装记录（来源、哈希、时间、布局、真实体积）
      .staging-*\                    ← `archive` 的临时目录，落位前短暂存在
    current → versions\<version>     ← junction，当前生效的那个
```

三条硬要求（决策 48），每条都有一个具体理由：

1. **载荷目录里不许放我们自己的文件。** 记录是 `<version>.json` 而不是
   `<version>/tuoen.json`。`layout.home` 会成为 `JAVA_HOME` 这类变量的值，
   而一个 JDK 根目录里多出一个不认识的 JSON 是**某些工具真的会去扫**的东西。
   验收里有一条断言专门盯这个（§9c：载荷顶层 15 个条目，`多出来的` 0 个）。
2. **目录是事实来源，记录只是附加信息。** 记录缺失/损坏时版本照样列出
   （只是说不出它从哪来）；只有记录没有目录 = 没装。于是"用户手删了一个版本目录"
   是自愈的，而"记录文件被编辑器写坏了"不会让整个存储不可用。
3. **`.staging-*` 与 `.incoming` 不是版本。** `archive` 的原子性正来自
   "在同卷临时目录里解压完再一次 `rename`"，所以 `versions/` 里**正常情况下**
   就会短暂出现 `.staging-node-4831`。不跳过的话用户会在版本列表里看见它。
   这是**跨 crate 契约**：`store` 跳过、`archive` 产生，两边都写了注释指向对方。

---

## §7 测试分布（`cargo test --workspace` = **463 passed / 0 failed**）

| 目标 | 数量 |
|---|---|
| `tuoen-cli` bin 单测 | 50 |
| `manage_contract`（本票新增，进程边界） | 23 |
| `catalog_contract` / `cli_contract` / `detect_contract` / `real_machine_acceptance` | 13 / 10 / 13 / 5 |
| `tuoen-store` 单测 + `store_ops` 集成 | 23 + 22 |
| `tuoen-archive` 单测 + `evil_archives` | 53 + 26 |
| `tuoen-core` | 52 |
| `tuoen-download` 单测 + `loopback` | 47 + 4 |
| `tuoen-manifest` | 65 |
| `tuoen-platform` | 54 |
| `tuoen-shim` | 2 |

门禁：`cargo clippy --workspace --all-targets -- -D warnings` 退出码 0；
`cargo fmt --all --check` 退出码 0。

**测试不碰真实机器状态**：整轮测试跑完之后 `%LOCALAPPDATA%\tuoen`
**仍然不存在**（本轮实测确认）。做法是 `crates/cli/tests/common/mod.rs` 的
`IsolatedHome` —— 把 `LOCALAPPDATA` / `APPDATA` 指向临时目录。
它能work是因为 `Store::at_default_location()` 读的是**环境变量**而不是
Win32 的 `SHGetKnownFolderPath`：读环境变量让"存储位置"成为一个可注入的输入，
于是最高层那条 seam 在测试里也可用。

---

## §8 诚实的未覆盖项

- **junction 创建失败时的降级路径没有被真机触发过。** 代码里有
  （`Repoint::Degraded`，当 `current` 原位置是 symlink 时走删了重建），
  契约测试也覆盖了 `--json` 里的表现，但**没有一台机器上真的走到过那条路** ——
  造它需要先手工把 `current` 做成 symlink，而本机未提权 + Developer Mode 关闭
  **做不出 symlink**（ADR-0001）。这条在"新机器上第一次跑"之前都算未验证。
- **`uninstall --force` 之后"删载荷失败"这个分支**只在 `store` 的单测里覆盖，
  真机没触发过。
- **并发**：两个 `tuoen install` 同时跑同一个版本时，`archive` 的
  `DestinationExists` 会挡住第二个，但**没有实测过**（需要真并发）。
- **`.7z` 制的安装路径**只走过 `archive` 的真机验收（票据 #5），
  没有从 `install` 端到端跑过一次 —— 因为种子目录里的 node/temurin 都是 `.zip`。
- **`npm.cmd` 是通过测试脚本用 `&` 调起来的，不是通过 shim。** 票 #7 才发 shim；
  这一票的判据是"存储里的版本能跑"，不是"`PATH` 上敲 `node` 能得到我们这份"。

---

## §9 这一票**没有**做的事（刻意的边界）

- **没有改 `PATH`。** 存储里的版本要生效还得靠 `PATH` + shim，那是票 #7 / #8。
  这一票只提供 `current` 这个稳定路径给它们指。
- **没有发 shim。** 同上。
- **没有自动激活。** `install` 不翻 `current`（决策 49）。理由：本项目的招牌是
  多版本共存，而顺手激活意味着"装一个旧版本去做兼容性排查"会**静默改变正在
  生效的环境** —— 而且改得没人知道（已经开着的终端、IDE、构建脚本拿不到）。
  代价是安装成功的输出**必须把下一步命令原样打出来**，验收里有断言盯它。
- **没有"更新到最新版"这类命令。** `install` 一个更高的版本 + `use` 就够了；
  专门的 `upgrade` 是 L1 之后的事。
