# L0-09 真机验收：在作者本机上真的装一个工具链并让它生效

**这一票不写新功能，它写的是证据。** 证明 L0 不是"框架搭好了"，而是"真的能装东西"。

- 验收脚本：`scripts/acceptance-L0-09.ps1`（33 项期望，退出码跟随失败数；`-SelfTest` 必 exit 1）
- 只读的新终端模拟：`scripts/fresh-terminal.ps1`
- 机器工件（原始输出，没有手写数字）：`docs/acceptance/L0-09-real-machine-run.txt`

跑法：

```powershell
pwsh -File scripts/acceptance-L0-09.ps1          # 33/0，exit 0，约 12 秒（缓存命中时）
pwsh -File scripts/acceptance-L0-09.ps1 -SelfTest # 33/1，exit 1
```

---

## 1. 逐条验收

| # | 标准 | 判据 | 证据 |
|---|---|---|---|
| ① | `tuoen install node@<ver>` 成功，`list` 正确显示 | 退出 0；`list --json` 的 `installedVersions` 含新版本 | 真装 `24.21.0` 与 `24.20.0` 各一次；`list` 显示三个版本、`*` 落在生效的那个上 |
| ② | 全新终端里 `node -v` 解析到我们装的版本，**或** tuoen 明确报告遮蔽并给下一步 | 在注册表口径的 PATH 里看 `where node` 的第一个命中 | 本机**被遮蔽**（机器级 `C:\nvm4w\nodejs`）→ 走第二条路：`path add --dry-run` 报出 **4 条**被抢、点名目录与命中的文件、给出下一步 |
| ③ | `tuoen use` 切版本后新终端里生效；已开着的终端的行为被如实记录 | 同一批 shim 文件报出新版本；shim 文件的长度/时间戳不变 | `24.19.0 → 24.21.0 → 24.19.0` 三次切换，`node.exe` 依次报 `v24.19.0 / v24.21.0 / v24.19.0`，**4 个 shim 文件的 mtime 一个都没变**（决策 11 在真机上成立） |
| ④ | `tuoen uninstall` 后系统状态干净 | 版本目录没了；`%TEMP%` 与存储里没有 `.staging-` 残留 | `uninstall node 24.21.0` → 释放 1994 文件 / 106986507 字节，存储只剩 `24.19.0`；残留检查 0 |
| ⑤ | `--dry-run` 的预览与真实执行结果**一致**（真对比，不是声称） | 两次的 `data.plan` 对象**逐字节相同** | `install node@24.21.0` 与 `install node@24.20.0` 各比一次，都是 `True` |
| ⑥ | 全过程未使用 `setx`，`PATH` 未被静默改写 | 源码里带引号的 `"setx` 命中 0 处；两个作用域 `Path` 的 raw+类型+SHA256 跑前跑后逐字节相同 | 脚本 §1/§7；本次验收前后两个作用域都是**逐字节相同**（用户级 773 字符 sha `51AEEE76…0755`，机器级 1007 字符 sha `9C398A20…F6ED`） |
| ⑦ | 未触碰 nvm4w 的符号链接、环境变量或状态 | 链接目标 + `NVM_HOME`/`NVM_SYMLINK` 跑前跑后逐字符相同 | `C:\nvm4w\nodejs -> …\nvm\v24.19.0`（tag `0xa000000c`），两个变量原样；`C:\nvm4w\nodejs\node.exe -v` → `v24.19.0` |
| ⑧ | 发现的问题**各自成为新 issue**（带复现步骤） | 三个新 issue：#19 / #20 / #21 | 见 §4 |
| ⑨ | 实测数据写入 issue 评论：安装耗时、shim 启动开销、`PATH` 长度变化 | 见 §3 | — |
| ⑩ | `cargo test` 与 `cargo clippy -- -D warnings` 通过 | 见 §6 | — |

---

## 2. 真机上发生了什么

### 2.1 装

```
install node@24.21.0   墙钟 1.46s    命中下载缓存（制品在 22:04 就缓存过了）→ 不产生网络流量
install node@24.20.0   墙钟 156.14s  冷缓存：cn-npmmirror 150.4s 失败 → cn-tencent 4.05s 成功
```

**两条都必须报出来，只报 1.46 秒是误导。** 缓存命中是设计内的正常路径（票据 #4 的验收项），
它不是"安装有多快"的答案。冷的那一次才是，而它暴露了一条真问题（#19）。

镜像加速是真的在工作：两次都**没有**走 `nodejs.org`，走的是
`cdn.npmmirror.com` / `mirrors.cloud.tencent.com`。换源也真的救回来了
（`failuresBeforeSuccess: 1`，最终哈希校验通过）。

### 2.2 让它生效

**先说结论：在这台机器上，`node` 的名字冲突我们输掉了 —— 而且 tuoen 说清楚了。**

一个全新终端（注册表口径的 PATH = 机器级 `;` 用户级，49 条）里：

```
C:\nvm4w\nodejs\node.exe                          ← 机器级，先命中
C:\Users\Muelsyse\AppData\Local\tuoen\shims\node.exe  ← 我们的，在后
v24.19.0
C:\nvm4w\nodejs\npm
C:\nvm4w\nodejs\npm.cmd
C:\Users\Muelsyse\AppData\Local\tuoen\shims\npm.exe
```

`tuoen path add <shim 目录> --dry-run` 对同一件事的预报：

```
遮蔽：我们发布的命令里有 **4 条**被别的目录抢在前面（一共 4 条）。
  ! `corepack` → 先命中的是 `C:\nvm4w\nodejs\corepack.cmd`（机器级）
  ! `node`     → 先命中的是 `C:\nvm4w\nodejs\node.exe`（机器级）
  ! `npm`      → 先命中的是 `C:\nvm4w\nodejs\npm.cmd`（机器级）
  ! `npx`      → 先命中的是 `C:\nvm4w\nodejs\npx.cmd`（机器级）

要修只有两条路：
  · 抢在前面的是**机器级**条目 → 要管理员权限：系统属性 → 环境变量，把那条移到最后或删掉。**tuoen 不做顺手提权**，也不会替你改机器级。
```

**这里有一个必须写下来的陷阱：`node -v` 报的 `v24.19.0` 是 `C:\nvm4w` 的 node 报的，不是我们的。**
两边的版本号**恰好一样**（我们的 store 里也装着 24.19.0），于是"版本号对上了"看起来像成功。
判据只能是 `where node` 的第一个命中，**版本号本身不是证据**。

把遮蔽者从模拟里去掉之后（`-RemoveEntry C:\nvm4w\nodejs`，只改子进程的环境块），
同一个 `node.exe` 就赢了，而且 `use` 一翻它就跟着换：

```
生效版本 24.19.0 → where node 命中我们的 shim → node -v = v24.19.0，npm -v = 11.17.0
生效版本 24.21.0 → where node 命中我们的 shim → node -v = v24.21.0，npm -v = 11.19.0
生效版本 24.19.0 → where node 命中我们的 shim → node -v = v24.19.0，npm -v = 11.17.0
```

`npm -v` 从 `11.17.0` 变成 `11.19.0` 是一条**独立证据**：它证明换掉的真的是跑起来的那份载荷，
而不是某一个被缓存的版本字符串（两个 Node 版本自带的 npm 版本不同）。

### 2.3 为什么必须做"新终端模拟"，而不是在当前 PowerShell 里再敲一次

环境块是在 `CreateProcess` 时从父进程复制的，**注册表只在 shell 建立自己的环境时被读一次**。
所以在当前这个 PowerShell 里再跑一次 `where node`，拿到的还是它启动时那份旧环境 ——
它能证明的东西**恰好是零**。

`scripts/fresh-terminal.ps1` 从注册表**重新读一遍**两个作用域的 `Path`（不展开），
拼成 `机器级 ; 用户级`，只把它塞进子进程的环境块。这正是登录时 `explorer.exe` 做的事。

它**不还原**两件事，都写进脚本头注释了：
MSIX 包激活时由启动器注入的条目（本机 PowerShell 会注入它自己的别名目录，而且**排在机器级之前**）、
以及祖先进程自己改过的环境（从 cargo 里开出来的终端会多几条）。
这两条都只会**增加**条目，不会改变"机器级在用户级之前"这个顺序 ——
而本机实测确认了这一点（ADR-0002 的依据）。

---

## 3. 实测数字

| 项 | 数字 | 怎么量的 |
|---|---|---|
| 安装（缓存命中） | **1.46 s / 1.44 s** | 外层 `Stopwatch` 包住整条 CLI 调用，两次独立跑 |
| 安装（冷缓存，跨源） | **156.14 s**（失败的源 150.44 s + 成功的源 4.05 s + 解压落盘） | 同上；`attempts[]` 里是 CLI 自己量的每源毫秒数 |
| 载荷规模 | 归档 37 618 919 / 37 539 751 字节 → 解压后 106 986 507 / 106 774 079 字节，2458 条目 / 1994 文件，最大深度 10 | `install --json` 的 `result.audit` |
| `node --version` 直跑 | **21.48 / 23.14 / 23.29 ms**（中位，三次独立测量） | 同一进程内 20 轮、去掉前 4 轮预热取中位 |
| `node --version` 经 shim | **33.26 / 33.61 / 33.69 ms** | 同上 |
| **shim 启动增量** | **10.4 – 11.8 ms** | 两臂在同一个进程里量，PowerShell 自身的开销两边都出现、差值才是 shim 的 |
| `npm --version` 经 shim | 130.04 ms | 同上（对照：票据 #7 实测 `npm.cmd` 直跑 233.8 ms —— **口径不同，不许直接下结论**） |
| shim 文件大小 | 287 744 字节 / 条（release 模板） | `Get-ChildItem` |
| 生成 4 条 shim | 1 257.8 ms（debug CLI） | `Stopwatch` |
| 用户级 `PATH` 变化 | 773 → 817 字符（+44，就是那条目录），类型 `REG_SZ` 不变 | `path add` 的输出 + 注册表 raw 读数 |
| 计划里的长度口径 | `1780 → 1824 字符`（**注册表口径的下界**，不含进程注入） | `path add` 的输出 |
| 生效 PATH 长度 | 1781 字符（进程那条）→ 模拟里 1825（注册表拼接）→ 去掉遮蔽者并追加 shim 目录后 1793 | `fresh-terminal.ps1` 的 `PATH_CHARS` |
| 删除一个版本 | 362 ms / 347 ms（释放 1994 文件 / 106 774 079 与 106 986 507 字节） | `Stopwatch` |

**启动增量的口径要说清楚**：PowerShell 自己拉起一个进程也要花十几毫秒，所以这张表里的绝对值
**包含了 PowerShell 的开销**；两臂相同，所以**差值（10–12 ms）才是 shim 的**。
票据 #7 用 Rust 的 `Instant` 在外层量过同一件事（增量 9–11 ms），两条独立测量互相印证。

参考值"Scoop 原生 shim 33–36 ms"是**总量**口径 —— 我们的总量也就在 33 ms 上下，
但**两者测法不同，不许宣称"同口径打赢"**。

---

## 4. 这一票发现的问题（各自成了新 issue）

### 4.1 源失败要等 150 秒才换源 → [#19](https://github.com/Muelsysel/tuoen/issues/19)

`curl: (56) schannel: server closed abruptly (missing close_notify)` 之后才换源。
换源逻辑是对的，代价不对：**150 秒里有 149 秒是白等的**，第二个源只用了 4 秒。
这是本机网络路径（本地 TUN/fake-ip 代理）很容易触发的形态，不是一次性意外。

### 4.2 `path remove` 拒绝摘掉 shim 目录，而且没有退路 → [#20](https://github.com/Muelsysel/tuoen/issues/20)

`path add <shim 目录>` 退出 0，`path remove <shim 目录>` 退出 1（`protected-shim-dir`），
`--help` 里**没有任何退路开关**。于是：**tuoen 自己加的东西，tuoen 自己收不回来**，
用户被推回"手工改环境变量"——那正是这个项目存在的理由。
这条是**在验收标准④上直接踩到的**（不修的话，"`PATH` 无残留条目"永远只能靠手工核对）。

### 4.3 `shim remove` 与 `path remove` 的幂等语义不一致 → [#21](https://github.com/Muelsysel/tuoen/issues/21)

要删的东西本来就不在时：`path remove` 说"什么都没有改"并退出 0，`shim remove` 说
"N 个名字里有 N 个没删成"并退出 1。清理脚本第二次跑就会把"什么都没做"报成"出错了"。

**这三条都记在 issue 里，没有在本票里偷偷绕过。**

---

## 5. 真的写了一次用户级 `PATH`（手工那一次）

验收脚本**不做**真实 `PATH` 写入 —— 因为 §4.2 那条问题：它加得进去、收不回来，
所以脚本自己也没有干净的退路。真写一次的证据是**手工**跑出来的，记录如下。

### 5.1 写的时候是什么样

```
把 `C:\Users\Muelsyse\AppData\Local\tuoen\shims` 追加到**用户级** `PATH` 的末尾。
长度：1780 → 1824 字符（**注册表口径的下界**，不含进程注入；档位 ok；还剩 6367 字符）
  + 追加 `C:\Users\Muelsyse\AppData\Local\tuoen\shims`（第 16 段之后）
✓ 已写入用户级 `PATH`：817 字符，类型 sz。
  广播 `WM_SETTINGCHANGE`：0 个顶层窗口应答 —— 它只说明广播发出去了，**不是「所有进程都更新了」**。
```

写完之后**独立地**用注册表 API 核对（不经过我们的代码）：

| 核对项 | 结果 |
|---|---|
| 用户级长度与类型 | 817 字符 / `String`（写前 773 / `String`） |
| 尾部正好是 `;<shim 目录>` | `True` |
| 前缀 773 字符的 SHA256 | `51AEEE76B367D42E3C668AB165D4612D9B8C0267C754CAA1F4CADB570D600755` = **写前那一份** |
| 机器级 SHA256 | `9C398A20981EE862114899A1B489A0C7611A11678CC31DBDD003C028C8F8F6ED` = 写前那一份 |

也就是说：**写进去的就是"原来那条 + `;` + 我们的目录"，一个字节没多、没少、没被展开、类型没变。**

### 5.2 复原，以及它暴露的问题

`tuoen path remove <shim 目录>` **拒绝**（退出 1，`protected-shim-dir`）。复原是**手工**做的：
把尾部那段去掉、用同一个类型写回，然后核对 SHA256 **等于写前那一份**（`True`）。

手工那一步是这个流程里**唯一**一次不由 tuoen 执行的写入，而它恰好证明了 §4.2 值得修：
用户被要求手工做的事，正是本项目想消灭的失败模式。

---

## 6. 诚实未覆盖

1. **真实的"这个终端是 explorer 开出来的"没有测。** 本机没有脚本化的办法让 explorer
   起一个终端；`fresh-terminal.ps1` 是**忠实模拟**（从注册表重建 PATH），不是端到端。
2. **MSIX 注入项没被模拟**（真实 PowerShell 终端里它会排在机器级之前）。它只会让顺序更不利，
   不会让结论变好。
3. **机器级 `PATH` 的写入完全没有做**，也没测 —— 那要提权，而 tuoen 刻意不做顺手提权。
4. **"新终端里生效"是在模拟里验证的**（把遮蔽者从子进程环境块里去掉 + 追加 shim 目录），
   注册表从头到尾只有那一次 `path add`。真实用户要做的是自己删掉机器级那条 —— 那一步不属于 tuoen。
5. **只测了 node。** `temurin`（JDK）在目录里有 recipe，但本机 `java` 也被机器级 Oracle
   `java8path` 遮蔽着，装它不会带来新的结论类别。
6. **只测了 Windows x64 + 本机这一种网络路径**（本地代理/TUN）。#19 那条 150 秒在其他网络下
   可能不复现。
7. **并发没有测**：两个 `tuoen` 同时写同一个版本、或同时翻转 `current`。
8. **`use` 对已经开着的终端的影响是"被记录"而不是"被验证"**：我们记录的是平台事实
   （环境块在 `CreateProcess` 时复制），没有去测量一个真实的长驻 IDE。
9. **下载缓存按设计保留**（这次多了 24.20.0 那一份 37 MB），没有命令化的清理入口。
   它不是残留，但也确实占地方。

---

## 7. 门禁与复现

```
cargo test --workspace                                 641 passed / 0 failed
cargo clippy --workspace --all-targets -- -D warnings   exit 0
cargo fmt --all --check                                 exit 0
pwsh -File scripts/acceptance-L0-09.ps1                 33 / 0，exit 0
pwsh -File scripts/acceptance-L0-09.ps1 -SelfTest       33 / 1，exit 1（证明退出码真的会变）
pwsh -File scripts/acceptance-L0-08.ps1                 57 / 0，exit 0（上一票没有被这一票跑坏）
```

**为什么 L0-08 这次是 57 而不是上次评论里的 58**：它的计数会随机器状态浮动 ——
上一次机器上还留着几条历史 shim，多走了"新生成的不能覆盖别人的东西"那一条期望。
现在这台机器是干净的，所以是 57。**两次都是 0 失败**，而且它从来不把"跳过"算成"通过"
（票据 #8 的教训）。

复现这一票：

```powershell
cargo build --release -p tuoen-cli -p tuoen-shim
pwsh -File scripts/acceptance-L0-09.ps1
# 想看"被遮蔽"那一段的真实形状：
pwsh -File scripts/fresh-terminal.ps1 'where node & node -v'
pwsh -File scripts/fresh-terminal.ps1 -RemoveEntry 'C:\nvm4w\nodejs' -AppendEntry (target\release\tuoen.exe shim path) 'where node & node -v'
```

**脚本跑完必须回到原样**：它在跑之前快照两个作用域的 `Path`（raw + 类型 + SHA256）与 nvm4w 的状态，
跑完再比一次，并把 `path_untouched=True nvm_untouched=True` 写进 `SUMMARY` 契约行。
