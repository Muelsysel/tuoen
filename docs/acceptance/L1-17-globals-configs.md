# 票据 #17 `tuoen capture --only globals|configs` —— 真机验收

- **机器**：作者本机（Windows、非提权、zh-CN），`node v24.19.0`、`pip 25.0.1`、`git` 两层都在。
- **日期**：2026-10-02/03。
- **结论**：`scripts/acceptance-L1-17.ps1` → **`checks_passed=117 checks_failed=0 checks_skipped=1 verdict=PASS`，exit 0**；
  `-SelfTest` → `117 passed / 1 failed`（那一条是故意失败的），**exit 1** ✓。
- **工件**：`L1-17-globals-configs-run.txt`（脚本逐行输出，145 行）、`L1-17-human-output.txt`（三份人类输出逐字，133 行）。
- **设计**：`docs/DESIGN.md` §1.20 的决策 **165–188**。

---

## 1. 逐条验收

### 1.1 票据"必须实现的"四条

| 票据要求 | 证据 |
|---|---|
| `globals.toml` 的 `tool_version` **必需** | 真机两行都有（`v24.19.0` / `3.12`）；`--no-version` 下键仍在、值 `unknown`（决策 172：键没有"省略"这个选项） |
| `prefix_inside_version_dir` **由代码判定**，不能靠用户填 | npm `true`（`C:\nvm4w\nodejs` 是 `SymbolicLink` → `…\nvm\v24.19.0`）、pip `false`（`Python312` 不是版本段、祖先无 reparse）；四支判据各有隔离固定装置（决策 173） |
| `configs.toml` 的 `captured = false` + **具体** `skip_reason` | 11 条跳过项，六种 kind，逐条带中文原因；**`.m2/settings.xml` → `contains-credential-shape`**（本票最重要的证据） |
| 必须同时读**系统级与全局级** git 配置并标注层级 | `configs.toml` 里两行（`layer = "system"` / `"global"`）+ `[git]` 表：`identity_source = "missing"`、两条路径都指向 `git` 自己报的那个文件 |

### 1.2 票据"必须有的用例"八条

| 票据要求 | 落点 |
|---|---|
| fixture 里的 `glpat-` token 不出现在任何输出文件、跳过清单里有它、`skip_reason` 具体 | `crates/core/tests/capture_globals_configs.rs` 的变异用例（把假 token 换成 `not-a-credential` → 判定从"跳过"翻成"捕获"）+ CLI 契约用例扫八个产出文件 |
| `prefix_inside_version_dir`：版本目录 → `true`，普通目录 → `false` | ③ 支单独为真的 `version-segment-prefix.toml`、② 支单独为真的 `ancestor-junction.toml`、四支全假的对照 |
| 同一个 npm 前缀在两个 Node 版本下 → **两份独立清单** | `two-node-versions/`（`v22.11.0` / `v24.19.0`，包集合不同） |
| 全局包命令失败 → 报告"发现但无法枚举"，**不报错退出** | `enumeration-fails/`：`command-failed` / `timed-out` / `bad-json` 三个 slug，行本身照样写出去（决策 175） |
| git 层级：系统级 + 全局级都读到并标注来源；缺失就明确说缺失 | `git-identity-layers/` 五个场景（system-only / global-only / global-wins / missing / git-unavailable） |
| 缓存目录进入跳过清单 | 真机 6/6 个（`pnpm\store` / `npm-cache` / `pip\Cache` / `uv\cache` / `.cache\codex-runtimes` / `.m2\repository`），且**不带体积**（我们不走进那 8 GB） |
| `content_hash` 幂等 | 真机两次捕获同哈希（脚本 §5）；固定装置 `idempotent/` 逐字节比 |
| 不可读文件 → `skip_reason = "unreadable"`，不崩溃 | `unreadable-too-large-binary/`（`unreadable` / `too-large` / `binary` 各一条 + 一条对照） |

### 1.3 票据"验收标准"五条

1. `cargo test --workspace` 全绿、clippy `-D warnings` 干净 → **1307 passed / 0 failed**（#16 之后是 1233），`fmt --check` 与 clippy 均 exit 0。
2. 真机跑 `--only globals` 与 `--only configs` 并把产出与跳过清单贴进 issue → 见 §3 与本文件两份工件。
3. **确认 `.m2/settings.xml` 被跳过且原因具体** → `captured = false` + `skip_reason = "contains-credential-shape"`，**且那一行既无 `bytes` 也无 `content_hash`**（跳过的行不许假装读过）。
4. 缓存目录出现在跳过清单 → 6/6 ✓。
5. `globals.toml` 的 npm 清单与取证报告对照（预期 7 个包）→ **逐字相同**（`@deepseek-ai/dsh@0.1.0-rc.6`、`@openai/codex@0.160.0`、`billion-context@0.1.179`、`corepack@0.35.0`、`npm@11.17.0`、`pnpm@11.21.0`、`tokentracker-cli@0.87.3`），且按 name 升序。

### 1.4 与票据草图的差异（**都是标点，不是语义**）

| 票据草图 | 实现 | 判据 |
|---|---|---|
| `packages = [{ name = …, version = … }]`（行内数组） | `[[global.packages]]` 子表 | 同一份数据的两种 TOML 写法，`toml` 对 `Vec<struct>` 的默认渲染是后者。**字段语义以票据为准，排版不是**（决策 172 的复核期澄清同源） |
| `tool_version = "24.19.0"` | `"v24.19.0"` | 决策 172：**存运行时自己报的那个字符串**（`node -v` 输出带 `v`）。票据举的例子把 `v` 去掉了，那是排版 |
| `path = "C:\\Users\\…"` | `path = 'C:\Users\…'` | `toml` 对含反斜杠的字符串用**单引号字面串**（字面串里反斜杠不转义，两者等价）。`pathdiff-cli` 的测试助手一开始只认双引号，就是这么红的 |
| `~/.ssh` 的 `known_hosts` "99 B" | 实测 **192 B** | 票据的数字是更早一次侦察的；本机今天就是 192 B。**机器是权威**，这条差异不影响任何判据（我们不捕获它，只记原因） |

---

## 2. 真机事实

### 2.1 脚本自己问出来的那一份（期望值的唯一来源）

```
PATH 口径：机器级 1007 + 用户级 773 = 1781 字符（注册表原文，不展开）
真实配置文件存在 4/7 个
       142  ac7272fdafa9  C:\Users\Muelsyse\.gitconfig
        88  7d60677a53c3  C:\Users\Muelsyse\.npmrc
       494  65c6110bc25b  C:\Users\Muelsyse\.m2\settings.xml
       353  f173396ef80b  C:\Users\Muelsyse\.docker\config.json
git 系统级配置：C:/Program Files/Git/etc/gitconfig
git 全局级配置：C:/Users/Muelsyse/.gitconfig
git 身份：两层都没有 user.name / user.email（本机事实）
独立事实：本机 .m2/settings.xml 里真的有一个 glpat- 形状的材料（本票的靶子）  长度 40（值不打印）
独立枚举：node=v24.19.0 npmPrefix=C:\nvm4w\nodejs npm 包=7 个；python=3.12 pip 包=3 个
缓存目录存在 6/6 个
JetBrains 产品目录 1 个；密钥材料文件 3 个
```

**这些数字全部由脚本自己**从注册表 / 磁盘 / 外部命令问出来（`node -v`、`npm config get prefix`、`npm ls -g --json`、`pip --version`、`pip list --format=json`、`git config --list --show-origin`），**不与产品输出对照** —— 与产品对照的是每一条断言。

### 2.2 两个分母（决策 158 的同一条规矩）

- **工具行**：本机 `tools` 是 **27** 行（新终端口径：机器级 + 用户级拼起来的 PATH），在含 `.cargo\bin` 的 shell 里会是 **29** —— 所以 `capture` 与 `restore` 必须**在同一个 PATH 口径下**跑，本文件与脚本 §1 都是新终端口径。
- **PATH 字符数**：机器级 1007 + 用户级 773 = **1781**（注册表原文，不展开）；`--only globals` 那一次没有捕获 `path`，所以这个数字只用于 §7 的"逐字未变"。

### 2.3 票据与本机的两处数字差异

- `known_hosts`：票据 99 B / 实测 192 B（见 §1.4）。
- 票据的缓存体积（pnpm store 4741.8 MB 等）是**另一次**测量的；本票的判据是"缓存目录出现在跳过清单里、且**不带体积**"，不依赖那个数字。

---

## 3. 真机演示（逐字）

### 3.1 `--only globals`

```
[[global]]
tool = "npm"
tool_version = "v24.19.0"
prefix = 'C:\nvm4w\nodejs'
prefix_inside_version_dir = true
…（7 个包按 name 升序，`[[global.packages]]` 子表）

[[global]]
tool = "pip"
tool_version = "3.12"
prefix = 'C:\Users\Muelsyse\AppData\Local\Programs\Python\Python312'
prefix_inside_version_dir = false
…（3 个包：pip 25.0.1 / pypdf 6.19.0 / pypinyin 0.55.0）
```

落盘**只有** `globals.toml` + `schema.toml`（`globals` 不产生跳过项，所以没有 `skipped.toml` —— 决策 179）。
人类输出（`L1-17-human-output.txt` 第 21–28 行）明说了那句本票的活陷阱：

> ⚠ 其中 1 个工具的全局前缀落在**按版本隔离**的目录里：换一个运行时版本，这些包会被**静默隐藏**（它们还装着，只是不在那个版本下）。

### 3.2 `--only configs` —— 本票最重要的证据

```
[[config]]
path = 'C:\Users\Muelsyse\.m2\settings.xml'
kind = "maven"
captured = false
skip_reason = "contains-credential-shape"
```

**这一行是本票的靶心**：它既没有 `bytes` 也没有 `content_hash` —— "跳过的行不许假装读过"。同一份清单里另外四条被正常捕获（`.gitconfig` / `.npmrc` / `.docker\config.json` + git 系统级那份），每一条的 `content_hash` 与 `bytes` 都与脚本**自己算的**逐字相同。

`[git]` 表：`identity_source = "missing"`、`system_config = "C:/Program Files/Git/etc/gitconfig"`、`global_config = "C:/Users/Muelsyse/.gitconfig"`，**缺身份时不出 `user_name` / `user_email` 键**（缺失就明确说缺失）。

### 3.3 跳过清单（11 条）

`cache-directory` ×6、`contains-credential-shape` ×1、`host-keys-not-captured` ×1、`credential-database` ×2、`private-key-file` ×1、`credential-named` ×1（`env` 那节的 `ARK_API_KEY`）。

关键的三条逐字（人类输出第 77、78、85 行）：

```
· …\.m2\settings.xml（configs / file）—— contains-credential-shape：文件内容里有一处键名含 `PASSWORD` 的赋值，值够长、不像路径 —— 无法排除它是凭据，所以没有写进快照
· …\.ssh\known_hosts（configs / file）—— host-keys-not-captured：known_hosts 记的是内网主机名，快照要提交进仓库
· …\IntelliJIdea2026.1\idea.key（configs / file）—— private-key-file：私钥文件不进快照（快照要提交进仓库）
```

注意第一条的**原因里说的是判据**（"键名含 `PASSWORD` 的赋值"），**不是材料** —— 这正是决策 178 要的形态：跳过原因必须具体到"为什么"，但绝不许把命中的值抄进去。

### 3.4 `restore` 遇到不认识的 section（决策 188）

把带 `globals.toml` / `configs.toml` 的快照交给 `restore`：`sections` **仍然只有四条**，而载荷里出现 `"unrestorable":["globals","configs"]`，人类输出两行（第 120–121 行）：

```
注意：这份快照里还有 **globals · configs** —— L1 的 `restore` **不还原**它们（只捕获）。
  这不是「没看见」：它们的内容在快照里（`globals.toml` / `configs.toml`）。还原它们要跑包管理器自己的解析、或者替用户写配置文件 —— 两件事 L1 都明确不做。
```

反例：一份只有四个旧 section 的快照，这个键**根本不出现**（两条契约用例分别钉住"缺席"与"空数组冒充缺席"，都做过变异）。

### 3.5 安全红线（脚本 §6）

- 真实 PAT 的**前 8 字符**与**全长窗口**不出现在任何产出文件里（本机 PAT 全长 40，值从不打印）。
- 产出文件里没有任何 `glpat-` 的**正文**（形状名后面只许跟标点/空格）。
- 仓库里没有 `glpat-` 字面量（GitHub push protection 防线，实测 0 处）。
- **零副作用**：两个作用域的 `Path` 原文与类型、整个 `HKCU\Environment`（14 个值）、四个真实配置文件的哈希、两个 `tuoen` 目录树、JetBrains 目录树 —— 跑前跑后逐项相同；用户目录里没留下 `globals.toml` / `configs.toml`。

---

## 4. 验收期发现的问题

### 4.1 一条是**我的期望错了**，产品是对的

`schema.toml` 的 `sections` 数组是**字典序**（`configs,env,globals,path,tools,wsl`），不是 `Section::ALL` 的写入顺序 —— 而我的脚本第一版按"顺序即写入顺序"断言，于是报了一条 FAIL。`SchemaFile::new` 从 #12 起就是排序的；**已经上线的行为不为一句文档措辞让路**：改的是决策 165 的措辞（"追加在末尾"说的是写入顺序，`schema.toml` 是排序的），不是实现。

### 4.2 四条是**脚本自己的坑**

1. **`PropNames` 判"键在不在"是空真。** 我的 Python 助手把每一行**规范化**成一个固定 dict（`tool/tool_version/prefix/inside/error/keys/packages` 每个字段都在），于是 `PropNames` 永远为真 → `--no-version` 下"pip 的 `prefix` 整键消失"这条断言**永远红**。产品其实是对的（落盘的 TOML 里 pip 那一段确实只有 `tool/tool_version/packages`）。修法：判键一律看 `keys`（= Python 侧 `sorted(r.keys())`，即 **TOML 原文的键名**）。同一个空真也藏在"npm 行永远有 `tool_version` 键"里。
2. **`setx` 判据自己命中自己。** 老脚本只扫 `crates/**` 所以没事，我这一版加扫 `scripts/**` —— 而"扫 `setx` 字面量"这件事本身必须在脚本里写出这个字面量（还有 35 处是守卫自己的注释与消息）。修法：产品源码按带引号的字面量扫（0 处）；脚本按**"会执行的语句"**扫 —— 去掉注释后要求 `setx` 后面跟着一个参数的样子（`[s]etx\s+[%/A-Za-z]`），正则写成 `[s]etx` 是为了**这段文字里没有 `setx` 子串**，它不会命中自己。两个方向都验证过：塞一条真的 `& setx TUOEN_PROBE 1` → 命中；纯文字提到 → 不命中。
3. **"产出文件里没有 `glpat-` 字面量"会在产品做对时假红。** 决策 178 要求跳过原因**说出形状的名字**（"值形似 GitLab 个人访问令牌（`glpat-` 前缀）"），那是一句人话里的判据，不是材料。`pathdiff-cli` 在自己的契约测试里先踩到，我这条是同一个错。修法：断言 `glpat-` **正文**（≥2 个字符）为 0 —— 形状名后面跟的是空格或右括号。真机上那条原因写的是"键名含 `PASSWORD` 的赋值"（另一种形状先命中），所以旧断言今天不会红，**纯属运气**。
4. **消息里的插值只是难看，不影响判定**：`"产品=$(@(Prop $x 'packages')).Count"` 会先把数组拼成串再跟上字面 `.Count`。判定用的是 `(...).Count` 的比较，所以那几条 PASS 是真的，但印出来的数字是空的 —— 已改成 `$(@(…).Count)`。

### 4.3 两处跨产物矛盾（裁决后才有一致的答案）

1. **pip 的 `tool_version` 存什么**：独立集成测试取整段 `(python 3.12)`，我的脚本取 `3.12`。裁**裸版本字符串**：字段名是 `tool_version`，值必须是一个版本；`(python 3.12)` 的括号与 `python` 是 pip 的**排版**。已写进决策 172 并加了一句判据："**'DESIGN 是权威'指的是设计意图，不是举例里的标点**"。
2. **pip 怎么调**：产品直接 spawn `pip.exe`，我的脚本走 `cmd.exe /C pip`。改脚本 —— **脚本要独立的是"答案"，不是"命令"**（判据：`pip --version` 的形状反推 prefix 这条独立算法必须自己算，但用哪条命令行不是判据的一部分）。

---

## 5. 诚实未覆盖

1. **`unsupported-output` 没有固定装置**（只有单测）。四种枚举失败里三种有端到端证据，这一种没有。
2. **"缓存目录绝不走进去"靠构造而非用例**：候选表是常量、只调 `inspect`，没有一条用例能证明"没读"。它证明的是"没打开文件"。
3. **决策 173 的 ② 支（任一祖先）只覆盖了"直接父目录是联接"**。多层由**代码阅读**验证（`MAX_ANCESTORS = 64`，到盘符根 `parent()` 为 `None` 时停），不是用例 —— 一个只查一层的实现仍然能过固定装置。
4. **固定装置是"按决策写的机器"，不是真机**：六个缓存目录、`.ssh` 形状、JetBrains 标记行全来自决策 180/181/182 的文字。若**决策表本身**漏了一项（真机上有第七个缓存目录），固定装置全绿也说明不了 —— 真机证据只有这份脚本。
5. **三条候选在真机上不存在**：`~/.ssh/config`、`~/.wslconfig`、VS Code `settings.json`。它们的端到端路径**没有被真机覆盖**（只有 fixture），真机跑的是"不存在 → 不出行"这条分支。
6. **git 身份只有 `missing` 这一种真机形态**：`system-only` / `global-wins` 只有 fixture 覆盖（本机两层都没有 `user.name`/`user.email`）。
7. **`unknown` 的合流**：`--no-version` 下 `tool_version = "unknown"`，与"这台机器上没有这个运行时"长得一样。区分它们要一个新字段，L1 不做（决策 172 的复核期补充）—— 那个开关是调用者自己给的，人类输出也会明说。
8. **`unrestorable` 只看文件在不在，不解析**：一个零字节的 `globals.toml` 也会被报成"这份快照里还有 globals"（决策 188 的已知限度）。
9. **JetBrains 产品目录的判据，实现是脚本的超集**（`GoLand2025.3.1` 那种形态实现也认）。我的脚本只做**单向**检查（脚本认出来的产品目录必须有标记行），所以实现多认一个不会红 —— 这方向是安全的，但它意味着"实现在这一点上比脚本更宽"没有反向证据。
10. **只有一台机器**：所有真机数字都来自作者本机（决策 158 的同一条限度）。

---

## 6. 门禁与复现

```
cargo fmt --all --check                      → exit 0
cargo test --workspace --no-fail-fast        → 1307 passed / 0 failed（#16 之后是 1233）
cargo clippy --workspace --all-targets -- -D warnings → exit 0
pwsh scripts/acceptance-L1-17.ps1            → checks_passed=117 checks_failed=0 checks_skipped=1 verdict=PASS，exit 0
pwsh scripts/acceptance-L1-17.ps1 -SelfTest  → 117 passed / 1 failed（故意的那一条），exit 1
```

复现前把 `cargo`/`dlltool` 放回 PATH（`$env:USERPROFILE\.cargo\bin` 与用户级 MinGW 的 `bin`）；
`capture` 与 `restore` 必须在**同一个 PATH 口径**下跑（§2.2 的两个分母）。
