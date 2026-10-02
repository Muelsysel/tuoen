# 票据 #5 验收：安全解压与原子安装

**对应 issue**：`Muelsysel/tuoen#5`
**真机证据**：`docs/acceptance/L0-05-archive-machine.txt`（退出码 0）
**设计依据（实测）**：`research/BSDTAR_SAFETY_MEASURED.md`
**实现**：`crates/archive/`

---

## 1. 验收项对照

| 票据要求 | 结果 | 证据 |
|---|---|---|
| 解压前对**每个条目**校验：规范化后在根内；拒绝绝对路径 / UNC / `..` | ✅ | `name.rs` 的 `validate_entry_name`；§4 的每一种变体都有独立用例 |
| 校验 symlink **条目目标** | ✅ **改成整个拒绝** | 见 §3；`non-regular-entry` |
| 拒绝保留设备名 `CON`/`PRN`/`AUX`/`NUL`/`COM1-9`/`LPT1-9` | ✅ | 含 `CON.txt`（带扩展名）与 `sub/NUL.txt`（子目录里）与 `COM¹`（上标） |
| 拒绝结尾的点和空格 | ✅ | `trailing.` / `trailing ` / `dir./x` |
| 拒绝名字里的 `:`（ADS 写原语） | ✅ | `stream.txt:evil` |
| 大小写碰撞检测 | ✅ | **在解压前查**（覆盖之后就看不出来了，见 §3） |
| 大小与条目数上限 | ✅ | `ExtractLimits`：条目数 / 总量 / 单文件 / 层数 / 单路径长度 |
| 拒绝时给出**具体原因** | ✅ | 15 个 `NameViolation` 变体，每个有稳定 slug + 中文解释；§4 每一行都点名了是哪一条 |
| 长路径用 `\\?\` 前缀处理 | ✅ | `long_path()`；真机装上了 **399 字符**的路径（§5） |
| **原子安装**：解压到临时位置，成功后一次移到 `versions/<ver>/`；失败清理 | ✅ | `StagingDir` + 一次 `rename`；§6 |
| 测试：Zip Slip **每一种变体**各一个用例 | ✅ | `tests/evil_archives.rs`，26 个用例，全部跑真 `tar.exe` |
| 测试：超长路径 / 含空格与版本号的路径 / 大小写碰撞 | ✅ | 三种都有；§5 |
| `.7z` 缺口有明确记录，且不影响其他格式 | ✅ **缺口不存在（实测推翻）** | §2 |
| `cargo test` 与 `cargo clippy -- -D warnings` | ✅ | 见 §8 |

---

## 2. `.7z` **不是**缺口 —— 这条推翻了原设计假设

`docs/specs/L0-install-engine.md:89` 与 `docs/DESIGN.md` 原文写的是
"`.7z` 是唯一需要内置库的缺口"。**实测证明这是错的**，而且不是靠手工敲
`tar -xf` 证明的 —— 是**走我们自己的代码路径**证明的：

```
=== 2. 真的装一个 .zip：node-v24.19.0-win-x64.zip ===
  上游哈希：57f71ab3652e797d84acddc79c81cc9ff1c6ddb2a1974cdb83f00fee9bff4c73
  取到：…（37304352 字节，来自 cn-npmmirror，缓存命中：true，0.2s）
  装上：…\versions\24.19.0-zip（2453 个条目，101.2 MB，审计 1.3s）
  node.exe：92825416 字节
  ✓ `node.exe --version` → v24.19.0（来自 node-v24.19.0-win-x64.zip）

=== 3. 真的装一个 .7z：node-v24.19.0-win-x64.7z ===
  上游哈希：64ab848053d7d055b66c0ca9ee94eccaa497e1f867de3216eaef6150ca82e075
  取到：…（23441591 字节，来自 cn-npmmirror，缓存命中：false，2.1s）
  装上：…\versions\24.19.0-7z（2453 个条目，101.2 MB，审计 2.4s）
  node.exe：92825416 字节
  ✓ `node.exe --version` → v24.19.0（来自 node-v24.19.0-win-x64.7z）

=== 4. 两个格式解出来的东西必须一致 ===
  .zip  解开 1989 个文件
  .7z   解开 1989 个文件
  ✓ 两个格式给出同样的文件数（1989）
```

**三条独立的判据同时成立**，所以这不是巧合：

1. 两个格式都给出 **2453 个审计条目 / 1989 个文件 / 101.2 MB** —— 一模一样；
2. 解出来的 `node.exe` 都是 **92825416 字节**；
3. **真的执行了它**：`node.exe --version` 报 `v24.19.0`。

第 3 条是这里最重要的一条。**大小对不代表能用** —— 一个漏了文件的解压
会给出正确的哈希与合理的大小，而程序跑不起来。只有真的执行才算数。

**结论：不需要内置任何 7z 库。** libarchive 3.8.8 自己就认它。

---

## 3. 三层防线，以及为什么一层都不能省

这不是"防御纵深"这种好听的话 —— 每一层都对应**一个实测出来的洞**。

### 第一层：我们自己校验条目名（`name.rs`）

**为什么必须有**：bsdtar 对**路径逃逸**可靠，对**名字的危险形态**完全不可靠。
下面每一条都是跑出来的：

| 归档里的名字 | bsdtar 的行为 | 后果 |
|---|---|---|
| `CON` / `sub/NUL.txt` | **真的创建**（用 `\\?\` 绕过 Win32 设备名解析） | 文件存在但**普通路径看不见**：`Test-Path '…\NUL'` = **False**，`Test-Path -LiteralPath '\\?\…\NUL'` = **True**，`dir /b /a` 能看到。**清不掉的残骸** |
| `trailing.` / `trailing ` | **真的创建**（13 / 15 字节） | Win32 会吃掉结尾的点与空格 → 资源管理器与多数工具里**打不开、删不掉** |
| `stream.txt:evil` | **静默改名**成 `stream.txt_evil` | 没写进 ADS，但**名字变了而没有任何提示** |
| `Readme.txt` + `README.TXT` | **静默覆盖**，只剩一个文件 | NTFS 大小写不敏感。安装场景里这意味着归档可以**偷偷替换掉自己的合法文件** |

**校验是"拒绝"，不是"净化"。** 净化（`CON` → `_CON`、`:` → `_`）看着更友好，
实际更危险：它让归档**声明一个名字、落盘成另一个**，而调用方与用户都无法知道
发生了什么 —— bsdtar 就是这么做的，而实测证明那会带来"名字变了没提示"。

判据很简单：**归档声明什么，就必须落成什么；做不到就整个拒绝。**

### 第二层：交给 `tar.exe`，并要求退出码为 0

**为什么必须有**：它比我们更懂格式，而且它自己的防线是有效的 —— 实测
`..`（各种拼法）、symlink（连指向根内的也拒）、硬链接、fifo、字符设备
**全部拒绝**，而 `C:\Windows\Temp\pwned.txt` **没有被写**。

**为什么还要"退出码必须为 0"**：实测**只要跳过了任何东西，退出码就是 1**
（`tar.exe: Error exit delayed from previous errors`）。非零退出码意味着
磁盘上是**半个目录** —— 那正是票据要禁止的东西。所以非零一律当失败，
由调用方清理。

**一个反直觉的发现**：经典的**两步 symlink 攻击**（先建指向根外的 symlink，
再往里写文件）在这台机器上**没有逃逸** —— symlink 被拒，随后的
`escape-link/pwned.txt` 被当成**根内的普通目录 + 文件**创建。

### 第三层：审计落盘结果（`audit.rs`）—— **权威判据**

**为什么必须有**：`tar.exe -tf` 的输出是**有损的**。

* 实测 bsdtar 把名字里的控制字符**转义**成 `\n` / `\r`，所以列表里看不出真相；
* 而**这个转义是双向的**：真目录 `bin` + 文件 `node.exe`，与"名字里含换行的
  `bin<LF>ode.exe`"，在 `-tf` 里**长得一模一样**；
* `-tf` 也**看不出条目类型**（`escape-link` 这个 symlink 在它眼里就是一行字）；
* bsdtar 还会把 `:` **静默改名**，所以磁盘上的名字可能与列表里的不同。

> **列表是意图，磁盘是事实。**

所以这一层走一遍真实目录，查五件事：① reparse point（结果里不该有任何链接）；
② **普通路径看不见的名字**；③ 磁盘上的真实名字（用与解压前**同一套**判据）；
④ 大小写碰撞（在**同一层目录的兄弟之间**比）；⑤ 条目数与总体积。

---

## 4. 恶意归档：每一种变体一个用例

真机输出（`L0-05-archive-machine.txt` §5）：

```
  ✓ `..` 被拒：unsafe-entry / parent-traversal（条目名里有 `..`），磁盘干净
  ✓ `/escaped-abs.txt` 被拒：unsafe-entry / absolute-path（条目名是绝对路径），磁盘干净
  ✓ `//server/share/unc.txt` 被拒：unsafe-entry / unc-path（UNC 网络路径），磁盘干净
  ✓ `NUL.txt` 被拒：unsafe-entry / reserved-device-name（保留设备名），磁盘干净
  ✓ `trailing.` 被拒：unsafe-entry / trailing-dot-or-space（结尾的点），磁盘干净
  ✓ `stream.txt:evil` 被拒：unsafe-entry / alternate-data-stream（`:`（ADS 写原语）），磁盘干净
```

**"磁盘干净"这四个字是每一条都要过的断言**：目标版本目录不存在，
`versions/` 下没有任何 `.staging-` 残留。判决对了但留下残骸，等于没挡住。

集成测试（`tests/evil_archives.rs`，26 个用例）覆盖面更宽，每种变体**各一条**：

| 用例 | 断言 |
|---|---|
| `a_parent_traversal_zip_is_rejected` | `parent-traversal` |
| `a_backslash_parent_traversal_zip_is_rejected` | `..\escaped.txt` —— **只查 `/` 的实现会放过它** |
| `an_absolute_path_zip_is_rejected` | `absolute-path` |
| `a_unc_path_zip_is_rejected` | `unc-path` |
| `a_drive_letter_zip_is_rejected` | `C:/escaped-drive.txt` |
| `a_reserved_device_name_zip_is_rejected` | `sub/NUL.txt` |
| `a_trailing_dot_zip_is_rejected` / `a_trailing_space_zip_is_rejected` | `trailing-dot-or-space` |
| `an_alternate_data_stream_zip_is_rejected` | `alternate-data-stream` |
| `a_control_character_zip_is_rejected` | `evil\n../escaped.txt` → `control-character`（见 §7） |
| `a_case_collision_zip_is_rejected_before_extraction` | `case-collision`，且点名是哪两个 |
| `a_zip_with_a_symlink_entry_is_rejected` | zip 的 Unix 模式 symlink 条目 → `non-regular-entry` |
| `a_tar_with_a_symlink_is_rejected` / `_hard_link_` / `_fifo_` | `non-regular-entry` |
| `an_unknown_extension_is_reported_as_unknown_not_as_unsupported` | `unknown-format`（措辞刻意不是"不支持"） |

**全部跑真实 `tar.exe`，全部在测试自己造的临时目录里，不碰机器的任何东西。**

### 为什么"校验 symlink 条目目标"变成了"整个拒绝"

票据原文要求"校验 symlink **条目目标**"。实测给出的答案是**不该去校验目标**：

* 在 Windows 上 bsdtar **根本不创建 symlink**（连指向根内的也拒）；
* zip 里带 Unix 模式 `S_IFLNK` 的条目，bsdtar 会把它落成一个**普通文件**，
  内容是那串目标路径的文本 —— **安全，但结果与归档声明不符且无提示**。

一个"声称是链接、实际是文本文件"的条目不该被当成正常安装。
而"校验目标然后允许它"会让我们**比 bsdtar 更宽松**，那是错的方向。
所以：**拒绝，并说明理由。**

---

## 5. 长路径与难路径

```
=== 6. 长路径与难路径 ===
  ✓ 含空格与版本号的路径装上了：…\hard-versions\spaces\IntelliJ IDEA 2026.1.1/bin/idea64.exe
  ✓ 长路径装上了（399 字符 > 260）：能读回 4 字节
```

两条都不是假想的：

* **含空格 + 版本号**：本机 `PATH` 上就有
  `C:\Dev\IDE\IDEA26\IntelliJ IDEA 2026.1.1\bin` —— 48 条 PATH 里 14 条含空格
  （29%），而这一条是最糟的（同时含空格和版本号）。
* **399 字符**：超过了 `MAX_PATH`(260)。能解开的原因是
  `long_path()` 会给绝对路径加 `\\?\` 前缀。

### `\\?\` 前缀的规则（每一条都有理由）

| 情况 | 处理 | 为什么 |
|---|---|---|
| `C:\a\b` | `\\?\C:\a\b` | 本地绝对路径 |
| `C:/a/b` | `\\?\C:\a\b` | **`\\?\` 只认反斜杠**，正斜杠必须换掉 |
| `\\server\share\x` | `\\?\UNC\server\share\x` | UNC 有它自己的形式 |
| 相对路径 `a\b` | **加不了** | 这就是"**相对路径永远受 260 限制**"那条平台事实 |

---

## 6. 原子性：拆成两条分别可验证的性质

"要么完整成功，要么完全不留痕迹"听起来像一句话，实际是**两件事**：

### 6.1 最终名字要么完整存在，要么根本不存在

做法：临时目录建在 `dest_root` **里面**（`<versions>/.staging-…`），
解压 + 审计全通过之后，才用**一次 `rename`** 把它搬到 `versions/<ver>/`。

**为什么临时目录必须在同一个卷上**：跨卷的 `rename` 会退化成"复制 + 删除"，
那就不再是原子的了。

**为什么 `commit` 拒绝覆盖已存在的目标**：覆盖要先删再搬，中间有一个
"什么都没有"的窗口。所以第二次装同一个版本会得到 `destination-exists`，
而**原来那份还在**（`installing_the_same_version_twice_is_refused_not_silently_overwritten`）。

### 6.2 失败时临时目录被删掉

真机输出 §7：

```
  ✓ 被拒：unsafe-result-path / too-large
    最终目录存在？false　临时目录残留？false
  ✓ 磁盘上什么都没有 —— "要么完整成功，要么完全不留痕"成立
```

**这一条测的是最难保证的那一半**：归档的名字**全部合法**（所以过得了第一层），
但总量超过上限 —— 于是失败发生在**解压之后、落位之前**。这正是
"解压到一半发现不对"的场景，也是唯一能验证"审计不通过时最终名字从未出现过"的场景。

### 6.3 清理为什么需要专门写一段代码

因为**我们刚刚证明了磁盘上会有删不掉的东西**（§3 的 `CON` 与 `trailing.`）。
`std::fs::remove_dir_all` 在**恰好是恶意归档**的情况下会失败 ——
而"清理失败"意味着磁盘上留着一个半成品目录，正是票据要禁止的东西。

所以 `remove_tree` 是两段式：**先用普通路径，失败了再走 `\\?\` 逐条删**。

两段式而不是直接上 `\\?\`：`\\?\` 会跳过**所有** Win32 规范化（包括把 `/`
当分隔符），对一个正常目录树用它反而更容易出错。正常路径优先，
只在它失败时才动用那把"不规范化"的钥匙。

---

## 7. 两个真 bug，以及一个"判决对了但理由错了"

### bug 1：目录条目带结尾斜杠 → **每一个真实归档都被拒**

实测 `tar.exe -tf` 把目录条目列成 `node-v24.19.0-win-x64/`（**带结尾斜杠**），
而每个真实 tar/zip 都有这一条。原来的校验把它切成一个空段 →
`EmptyOrDotSegment` → 归档被拒。

```
一个干净的归档应当装上: UnsafeEntry {
    entry: "node-v24.19.0-win-x64/",
    violation: EmptyOrDotSegment }
```

修法：结尾斜杠合法，去掉它再校验，并记住 `is_dir_entry`。
**这个标志还有第二个用处**：`strip_components` 会让"只有一段的顶层目录标记"
整个消失（那**完全正常**），而同样消失的如果是一个**文件**，那就是
"内容被静默丢掉" —— 两者必须区分。

**这个 bug 是被 `a_clean_zip_installs_and_leaves_no_staging_behind` 抓出来的。**
值得记一笔：**26 个恶意归档用例全绿，而唯一一个"正常路径"用例红了。**
只测攻击不测正常路径的话，这个 bug 会一路活到真机上。

### bug 2：`committed` 标志让成功路径留下残骸

我一开始给 `StagingDir` 加过一个 `committed: bool`，落位后置真、清理时短路。

**那是错的，而且错得很典型**：`committed` 表达的是"**payload 已经搬走**"，
但清理要问的是"**临时目录还在不在**" —— 两个不同的问题。

剥层时 payload 是临时目录的**子目录**，搬走它之后临时目录**还在**（空了但还在）。
于是那个标志让清理短路，磁盘上留下一个 `.staging-…` 目录 ——
正是"失败要完全不留痕迹"要禁止的东西，只不过这次是**成功路径**上留的。

修法：删掉那个标志。现在靠两条不需要标志的性质：剥层为 0 时 `commit` 搬的
就是临时目录**本身**（搬完 `!exists`）；剥层大于 0 时它还在（正常删掉）。

### 判决对了但理由错了：`evil\n../escaped.txt` 报成 `trailing-dot-or-space`

`-tf` 里一个 `\n` 有**两种**解释，而两种解释在列表里**长得一样**：

* 真的换行被转义（→ 真名里有控制字符，**危险**）；
* Windows 风格的分隔符（→ `bin\node.exe` 是**完全正常**的，7z 就是这么存的）。

原来一律按分隔符解释，于是 `evil\n../escaped.txt` → `evil/n../escaped.txt`
→ 段 `n..` 以点结尾 → 报 `trailing-dot-or-space`。**判决对了，理由完全错**，
而票据明确要求"给出具体原因，因为'解压失败'不足以判断是上游问题还是攻击"。

现在的规则（三种情况都验过）：

| 情况 | 行为 | 理由 |
|---|---|---|
| 分隔符解释**通过** | 接受它 | **不能反过来做**：把 `\n` 一律当控制字符会拒绝掉**全部 7z 制品**。真的是控制字符的话，第三层审计会在磁盘上看到真名并拒绝 |
| 不通过 + 名字里有转义序列 | 报 `control-character` | 真名里确实有控制字符，比"`n..` 以点结尾"准确 |
| 不通过 + 没有转义序列 | 报分隔符解释给出的那条 | `\e` 不是转义序列，所以 `..\escaped.txt` 就是纯粹的路径穿越 |

两条边界也验过：`\\` **不算**转义（否则 `\\server\share\x` 会报成
`control-character` 而不是 `unc-path`），`\7zip` **不算**转义
（三位八进制 `\ooo` 才是，否则正经路径被误拒）。

**教训**：`\n` 这个歧义是**不可判定的** —— 靠列表永远分不清。所以真正的
安全边界在第三层（磁盘），而第一层的诊断只该尽力而为。**诊断错了会误导人，
但不会放过攻击** —— 这个区分很重要。

---

## 8. 测试与门禁

```
cargo test --workspace        → 见 §8.1
cargo clippy --workspace --all-targets -- -D warnings  → exit 0
cargo fmt --all --check       → exit 0
```

`crates/archive` 自己的分布：

| 目标 | 数量 | 内容 |
|---|---|---|
| lib（`src/`） | 53 | 判据（`name`）、上限（`limits`）、`\\?\` 与清理（`staging`）、审计（`audit`，用 `FakeFileSystem`）、编排（`extract`）、解压器（`tar`） |
| `tests/evil_archives.rs` | 26 | **全部跑真实 `tar.exe`** |
| doc test | 1 | `lib.rs` 的用法示例 |

**测试不碰机器的任何东西**：归档、解压目录、版本目录全在测试自己造的临时
目录里。唯一被调用的外部程序是系统自带的 `tar.exe`，工作目录由我们指定。
**不写注册表、不写 PATH、不碰任何已安装的工具。**

### 8.1 全工作区

```
cargo test --workspace   → 364 passed / 0 failed
cargo fmt --all --check  → exit 0
cargo clippy --workspace --all-targets -- -D warnings  → exit 0
```

分布：archive 53、archive/evil_archives 26、cli bin 26、catalog_contract 13、
cli_contract 10、detect_contract 13、real_machine_acceptance 5、core 50、
download 47、download/loopback 4、manifest 65、platform 49、shim 2、doc test 1。

（票据 #4 时的总数是 284，本票新增 80。）

---

## 9. 这一区间发现并修掉的真 bug（都有测试守着）

| # | 症状 | 根因 |
|---|---|---|
| 1 | **每一个真实归档都被拒**（26 个恶意用例全绿时） | 目录条目的结尾斜杠被切出一个空段 → `EmptyOrDotSegment` |
| 2 | **成功路径上留下 `.staging-` 残骸** | `committed` 标志回答的是"payload 搬走了吗"，而清理要问"临时目录还在吗" |
| 3 | `evil\n../escaped.txt` 判决对但**理由错** | `-tf` 里 `\n` 的两种解释不可判定，原来一律按分隔符解释 |
| 4 | `\\server\share\x` 报成 `control-character` | `\\` 被当成了转义序列（它不是控制字符，而 UNC 是更准确的诊断） |
| 5 | `///` 报成 `unc-path` | UNC 需要 `//` 后面紧跟**非**分隔符；`///` 只是"以分隔符开头" |
| 6 | 退化名字 `/` 报成 `empty` | 退化名字该报"更准确"的那一条（以分隔符开头解释了为什么危险） |

第 1 条与第 2 条都是**正常路径**上的 bug，而不是恶意路径上的 ——
它们证明了"只测攻击"是不够的。
