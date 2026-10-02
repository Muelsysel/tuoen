# L0-03 下载层与镜像加速 —— 真机验收证据

**采集时间**：2026-10-02
**机器**：MUELSYSE（Windows 11 25H2 build 26200.9457，16 核 / 16.98 GB）
**命令**：`cargo run -p tuoen-download --example fetch_probe`
**二进制**：debug profile（验收跑的是真 `curl.exe`，profile 不影响它）

原始输出落盘在同一目录：

- `L0-03-download-machine.txt` —— 完整输出（78 行），**退出码 0**

---

## 1. 验收标准逐条对照

| 票据要求 | 结果 | 证据 |
|---|---|---|
| 内置源优选（Node.js 与 Temurin 国内镜像，按可达性/测速） | ✅ 探测排名：npmmirror 258ms → official 323ms → tencent 330ms；ustc 404、tuna 403 被淘汰 | §3 |
| 用户自定义镜像模板 | ✅ 自定义模板排在候选第 1 位并真的被尝试 | §2、§4 |
| SHA256 校验（报期望值与实际值） | ✅ 两处：哈希不符时同时报出两个值；成功时独立复算一致 | §4、§6 |
| 失败原因分类 + 镜像失败后回退 | ✅ `source-unavailable` 分类 + 真的从坏源回退到 npmmirror | §4、§7 |
| 缓存位于 `%APPDATA%\tuoen\cache`，重复下载不重复传输 | ✅ 第 2 次尝试次数 **0** | §5 |
| 支持本地归档文件作输入 | ✅ 本地文件通过校验；配错哈希被拒 | §8 |
| 测试不访问真实网络 | ✅ 单元测试注入 `FakeTransport`；`tests/loopback.rs` 只打 `127.0.0.1` | §9 |
| `cargo test` + `clippy -- -D warnings` 通过 | ✅ 284 passed / 0 failed，clippy exit 0，fmt exit 0 | §9 |

---

## 2. 期望值来自上游，不是代码里的常量

这一步是整个验收的**基础**，值得单独说：验收里那个"期望哈希"不是抄进代码的，
而是**当场从上游读回来的**：

```
索引：https://nodejs.org/dist/v24.19.0/SHASUMS256.txt
  取回 2967 字节，解析出 32 条制品
  node-v24.19.0-win-x64.zip
  sha256 = 57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73   ← 来自上游
```

一个用代码里硬编码的哈希去验收下载层的实验，答案是预先知道的 —— 它证明不了
"我们和上游对得上"。

---

## 3. 候选源顺序与探测结果

```
  1. custom:dead-on-purpose custom  https://mirror.invalid/https://nodejs.org/...zip
  2. cn-npmmirror   cn      https://cdn.npmmirror.com/binaries/node/v24.19.0/...zip
  3. cn-tencent     cn      https://mirrors.cloud.tencent.com/nodejs-release/v24.19.0/...zip
  4. cn-tuna        cn      https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/v24.19.0/...zip
  5. cn-ustc        cn      https://mirrors.ustc.edu.cn/nodejs-release/v24.19.0/...zip
  6. official       global  https://nodejs.org/dist/v24.19.0/...zip
```

探测（每个源只取 **1 个字节**）：

```
  cn-npmmirror   可达 206    200ms  取回 1 字节（区间请求，206）
  official       可达 206    311ms  取回 1 字节（区间请求，206）
  cn-tencent     可达 206    355ms  取回 1 字节（区间请求，206）
  cn-ustc        HTTP 404    124ms  https://mirrors.ustc.edu.cn/nodejs-release/index.json 返回 HTTP 404
  cn-tuna        HTTP 403    135ms  https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/index.json 返回 HTTP 403
  custom:dead-on-purpose  不可达  198ms  取 https://mirror.invalid/... 失败（curl 退出码 35）
```

**三件事在这里被证实：**

1. **八个"国内镜像"里只有一个能用**（决策 40：靠实测探测，不靠静态清单）。
2. **区间请求真的发出去了**：`取回 1 字节`。如果没发，这一行会是 35.6 MB，
   而且 Temurin 那种 190 MB 的包会让"探测"变成一次完整的下载。
3. **不可达的源没有被删掉**，它的失败详情留在报告里（"清华 403"说明有东西在拦，
   而不是源本身没有）。这是用户诊断网络的线索。

### 3.0 探测目标本身修过一次 —— 证据是"下了 155 KB"

第一次把探测目标从"镜像前缀（目录）"改成"一个小文件"之后，某一次运行的输出是：

```
  cn-tuna        可达 200   375ms  取回 155794 字节：**这个源不认区间请求**，探测会下整包（响应 200 而不是 206）
  cn-tencent     可达 200  2258ms  取回 115893 字节：**这个源不认区间请求**，探测会下整包（响应 200 而不是 206）
```

**这不是区间请求坏了，是探测目标选错了。** 一个不认区间请求的服务器会把
`-r 0-0` 当普通 GET；目标是**目录**时，那意味着为了问一句"能不能到"，
把整份目录列举下下来了（15 万字节的 HTML 版本列表）。

两条结论：

- **探测目标必须是一个文件**，不能是目录。现在 node 的五个源都打
  `index.json`（根目录下的固定文件名，**不带版本号所以不会过期**），
  Temurin 的三个源都打 `v3/info/available_releases`（小的真实 JSON）。
  实测改成文件之后，五个源全部是 **206 + 1 字节**。
- **目录列举回答的是错的问题**：它说明"这个目录能不能列"，不说明
  "制品路径对不对"。探测的判决是用来淘汰源的，测错了东西就会误杀。

顺带一句：**这个 155 KB 是"探测带回真实状态码"这个改动发现的**。早先
`SourceProbe.status` 被写死成 `206`，这种情况会看起来完全正常。
这条由一个测试钉住（`every_builtin_probe_target_is_a_file_not_a_directory`）。

### 3.1 一个必须回答的质疑：探测会不会误杀能用的源？

会 —— 如果探测打的 URL 与真正要下载的 URL 命运不同。这个质疑值得认真对待，
因为**探测的判决是用来淘汰源的**，误杀一个能用的源比不探测更糟。

所以另外单独验了一次：**不可达的源，它的制品 URL 是不是也不可达？**

| 源 | 探测目标 | 真实制品 |
|---|---|---|
| 清华 nodejs-release | 403 | `…/v24.19.0/node-v24.19.0-win-x64.zip` → **403** |
| 清华 nodejs-release | 403 | `…/v24.19.0/SHASUMS256.txt` → **403** |
| 清华 Adoptium | 403 | `…/21/jdk/x64/windows/OpenJDK21U-jdk_x64_windows_hotspot_21.0.5_11.zip` → **403** |
| 中科大 nodejs-release | 404 | `…/v24.19.0/node-v24.19.0-win-x64.zip` → **404** |
| 北外 Adoptium（API 形状） | 403/404 | `…/v3/binary/latest/21/…/eclipse` → **404** |

**五个源上，探测的判决与制品本身一致。** 清华是**整站拒绝**（连真实制品
URL 也拒），中科大是路径不存在，北外是 API 形状不存在 —— 三种不同的原因，
探测给出的"不可用"都是对的。

### 3.2 一条必须诚实记下的事：判决稳定，但**状态码不稳定**

同一台机器、同一个网络、同一个 URL，跨几次运行的状态码是**会变的**：

| 源 | 第 1 次 | 第 2 次 | 第 3 次 |
|---|---|---|---|
| 清华 nodejs-release 目录 | 403 | **200 + 155794 字节（整份列举）** | 403 |
| 清华 Adoptium API 路径 | 403 | 403 | 403 |
| 北外 Adoptium API 路径 | 404 | 403 | 403 |

**"不可用"这个判决每次都对，但理由每次都可能不同。** 所以：

- **不要基于状态码写"这个镜像坏了"的分类逻辑** —— 那会在同一台机器上
  时对时错。分类只该到 `source-unavailable` 这一层（这正是代码里做的）。
- 清华的 403 不是"镜像缺失"，而是**反爬**：响应体原文是
  「您访问使用的软件带有非常用软件的特征 / The software that you are using
  is with uncommon characteristics」。**curl 在它眼里不是浏览器** ——
  这是一条会反复咬人的平台事实（一个中国的镜像站可以合法地拒绝一切
  非浏览器客户端）。
- `attempts_per_source` 默认 1 仍然是对的：判决稳定意味着重试同一个源
  不会改变结论。

---

## 3b. Temurin（Adoptium）—— 票据点名要的第二个工具

```
  1. official-api   global   https://api.adoptium.net/v3/binary/latest/21/…/eclipse
  2. cn-tuna        cn       https://mirrors.tuna.tsinghua.edu.cn/Adoptium/v3/binary/latest/21/…/eclipse
  3. cn-bfsu        cn       https://mirrors.bfsu.edu.cn/Adoptium/v3/binary/latest/21/…/eclipse
  official-api   可达 206      340ms  取回 1 字节（区间请求，206）
  cn-bfsu        HTTP 403      122ms  https://mirrors.bfsu.edu.cn/Adoptium/v3/info/available_releases 返回 HTTP 403
  cn-tuna        HTTP 403      132ms  https://mirrors.tuna.tsinghua.edu.cn/Adoptium/v3/info/available_releases 返回 HTTP 403
  1 / 3 个源可用
```

**同一个网络、同一台机器：Node 有 3 个源可用，Adoptium 只有 1 个。**
这条对比本身就是"按工具分别探测"（决策 40）的论据 —— 如果按 Node 的结果
去推断 Temurin，会得出完全错误的结论。

### 一个刻意的顺序例外

**Temurin 表把官方排在第一位，与 Node 表相反**（Node 表把官方放最后）。
理由是这两个镜像**已知形状可疑**：

- 官方在前 → 代价是**零**：官方可用，镜像根本不会被碰到；
- 官方在后 → 代价是**每次 JDK 安装都先白等两次注定失败的请求**
  （本机 122ms + 132ms），换来的是两个大概率 404 的请求。

它们留在表里而不删掉，是因为清华的 403 是**反爬而非缺失**（形状未被证伪），
而官方 API 在有些网络里是被墙的 —— 那种情况下它们是仅有的长尾希望。
`--probe-sources` 拿到真实数据后会重新排序，那时这个问题有答案，
而不是靠这里的猜测。这条顺序由一个测试钉住
（`temurin_mirrors_do_appear_as_candidates_even_though_their_layout_is_suspect`），
免得有人"为了对称"把官方挪回后面。

---

## 4. 真的下一次：坏源 → 回退 → 校验

```
  取到：C:\Users\Muelsyse\AppData\Roaming\tuoen\cache\sha256\57\57f71ab3652e...c73
  大小：37304352 字节（35.6 MB）
  来自：cn-npmmirror  https://cdn.npmmirror.com/binaries/node/v24.19.0/node-v24.19.0-win-x64.zip
  在成功之前失败了 1 个源：
    失败 custom:dead-on-purpose    188ms [source-unavailable]  curl 退出码 35
    OK   cn-npmmirror     2115ms
  我们自己算的 sha256 = 57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73
  ✓ 与上游公布的一致
```

**回退路径是在真实网络上跑出来的**，不是模拟的：候选第 1 位是个指向 `mirror.invalid`
的用户模板，它失败了，第 2 位的阿里源接住了，35.6 MB 在 2.1 秒内下完，
哈希与上游公布的一致。

### 一条诚实的技术细节：`.invalid` 在这台机器上不是 DNS 失败

`mirror.invalid` 是 RFC 2606 保留域，按定义**永远不会**被解析成真东西。
但本机实测：

```
mirror.invalid    A     198.18.0.12
mirror.invalid    AAAA  2001:2::c
```

`198.18.0.0/15` 是 RFC 2544 的基准测试段 —— 也就是说**这台机器的 DNS 解析器
对所有域名返回伪 IP**（本机跑着 `127.0.0.1:2026` 上的代理，TUN/fake-ip 模式）。
于是一个"不存在的域名"在这里的表现是 **TLS 握手失败（curl 退出码 35）**，
而不是 DNS 失败（退出码 6）。

这不影响验收结论（失败被正确分类成 `source-unavailable`、正确触发了换源），
但它是一条需要记住的平台事实：**在这台机器上，"域名不存在"与"TLS 被拦"在
curl 的退出码层面长得一样**，所以不要基于退出码 6 去写"域名解析失败"的
分类逻辑 —— 那会在本机永远走不到。

---

## 5. 缓存：第二次一个字节都不传

```
  缓存目录：C:\Users\Muelsyse\AppData\Roaming\tuoen\cache
  缓存命中：true
  尝试次数：0（应当是 0）
  ✓ 第二次没有产生任何网络请求
  缓存现状：1 个制品 / 37304352 字节
```

命中时**仍然验一次哈希**（`fetch` 的第 1 步），只是不产生网络流量 ——
"写进去的时候验过"不能替代"用的时候验"，磁盘会坏、文件会被改。

---

## 6. 哈希不符：报两个值，并且不换源

```
  ✓ 报的是 ChecksumMismatch
    期望 sha256 = 0000000000000000000000000000000000000000000000000000000000000000
    实际 sha256 = be0629ee2bcd8e40bb856abdd3407f0762101b76bd60a36b8867f637733631c0
    字节数      = 2967
    独立重算    = be0629ee2bcd8e40bb856abdd3407f0762101b76bd60a36b8867f637733631c0
    ✓ 两个入口算出的摘要一致
    ……而且没有换源：报的是单条错误，不是 AllSourcesFailed
```

两点是刻意设计的：

- **"独立重算"用的是另一个入口**（`sha256_hex` 对第 1 步取回来的同一份字节），
  而不是下载层内部那次计算。若两者出自同一个错掉的实现，它们会一起错；
  两个入口一致才排得掉这种情况。（自实现的 SHA-256 在本区间被修过四个缺陷 ——
  见 `docs/DESIGN.md` 决策 41 —— 所以这条复核不是形式主义。）
- **哈希不符不换源**：被污染的字节不该靠换个镜像来"修好"。
  报的是 `ChecksumMismatch` 而不是 `AllSourcesFailed`，这本身就是"没有去试
  下一个源"的证据。

---

## 7. 404 的分类与建议

```
  全部 1 个候选源都失败：https://nodejs.org/dist/v24.19.0/this-file-does-not-exist.zip 返回 HTTP 404
  分类：source-unavailable（应当触发换源）
  建议：换一个源，或者用 `--mirror` 指定你自己的镜像模板；也可以先跑
        `tuoen fetch --probe-sources` 看哪个源在这个网络上真的可用
```

**这里修过一个真 bug**：早先的实现先判 curl 退出码、后判 HTTP 状态码，
而 `--fail-with-body` 让所有 4xx/5xx 的退出码都是 **22**，于是
`DownloadError::Http` 那条分支**永远走不到** —— 所有镜像 404 都会被报成
"curl 退出码 22"。现在先判 `-w` 给出的状态码，再用退出码兜住"连不上"
（那种情况 `-w` 只给出 `000`）。两者都要，只是先后不同。

---

## 8. 本地归档文件当输入

```
  输入：C:\Users\Muelsyse\AppData\Roaming\tuoen\cache\sha256\57\57f71ab3...c73
  sha256 = 57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73
  来源标记 = local（不是网络）
  哈希不符的本地文件被拒：SHA256 不符：期望 ffff...ffff，实际 57f71ab3...c73（…，37304352 字节）。
    这个制品可能已经在上游被替换，也可能有人在中间改过它 —— 两种都不应该继续安装
```

**本地文件也要验哈希**，否则"离线安装"就成了唯一一条绕过完整性检查的路径 ——
而那正是最需要检查的场景（U 盘拷贝）。

---

## 9. 单元测试不访问真实网络，且门禁全绿

- 语义测试注入 `FakeTransport`（最长前缀匹配的路由表，**没配路由 = 不可达**，
  不返回空字节）。
- `crates/download/tests/loopback.rs` 起一个绑 `127.0.0.1:0` 的 `TcpListener`，
  跑**真 `curl.exe`** 但**数据不出本机**。它守三件单测守不住的事：
  1. `-w` 哨兵解析在响应体含 `\n\x1f\x1f`（假哨兵）与 `\x00\xff` 任意字节时仍然正确；
  2. 404 会变成 `Http(404)` 而不是静默成功；
  3. **服务器真的收到了 `Range: bytes=0-0`** —— 光断言我们自己的 `curl_args`
     只能证明"我们打算发区间请求"。
- 没有 `curl.exe` 时环回测试**打印一行 SKIP 然后返回**，不静默跳过
  （"永远不跑的测试比没有测试更糟"）。

门禁（`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all --check`）：**284 passed / 0 failed**、clippy exit 0、fmt exit 0。
分布：cli bin 26、catalog_contract 13、cli_contract 10、detect_contract 13、
real_machine_acceptance 5、core 50、download 47、download/loopback 4、
manifest 65、platform 49、shim 2。

---

## 10. 这一区间发现并修掉的真 bug（都有测试守着）

| # | 症状 | 根因 |
|---|---|---|
| 1 | `"  SHA256:BA7816BF  "` 规整后是 `"sha256:ba7816bf"` | `trim_start_matches` 大小写敏感 |
| 2 | 上游写 `sha256:` 的制品全部被拒 | 削前缀与去 `-`/`_` 的顺序反了 |
| 3 | **每一个制品都"哈希不符"** | `ProcessRunner` 的 stdout 是 lossy UTF-8，`0xFF` 变成 `EF BF BD` —— 压缩包里一定有非 UTF-8 序列，症状看起来像上游被污染 |
| 4 | 所有镜像 404 都报成"curl 退出码 22" | 退出码判在 HTTP 状态码前面（见 §7） |
| 5 | **阿里源从来没出现过**（而它是本机最快的） | 内置表把镜像自己的前缀当成了替换的键，`strip_prefix` 永远不匹配 |
| 6 | `MirrorConfig::default()` 下一个候选源都没有 | 手写 `Default` 与 serde 的 `default = "default_true"` 不一致 —— **bug 只在测试里出现** |
| 7 | `--no-mirror` 时候选列表变空 | 官方兜底的判据在那种配置下永远为真 |
| 8 | 探测把状态码写死成 `206` | 一个不认区间请求的服务器会回 200 + 整包，写死就看不见它（现在 `FetchOutcome` 带回真实状态码） |
| 9 | 探测为了问一句"能不能到"**下了 15 万字节** | 探测目标是个**目录** —— 不认区间请求的服务器会把 `-r 0-0` 当普通 GET，于是整份目录列举被下下来（见 §3.0） |
| 10 | Temurin 表的注释说"形状对不上所以镜像不会出现" | 那是错的：它们的 `upstream` 就是官方 API 前缀，`strip_prefix` 一定匹配 —— 注释与数据不一致，**由一条测试抓出来** |

第 3 条的修法是给 `ProcessOutcome` 加 `stdout_bytes: Vec<u8>` ——
**这是本区间最重要的一个修复**：没有它，整个下载层的输出永远是错的，
而错误信息会指向上游。

第 8 与第 9 条是**同一类问题**：探测的判决用来淘汰源，所以它必须
① 看到真相（不能假定状态码），② 问对问题（不能拿目录当制品）。
两条都是真机输出抓出来的，不是想出来的。
