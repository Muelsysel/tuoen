# `fixtures/capture/**` —— `capture` 的 `globals` + `configs` 固定装置（票据 #17）

这些文件是 **`tuoen_platform::fixture::MachineFixture` 的 TOML 序列化**（`serde` +
`#[serde(deny_unknown_fields)]`），描述的是**一台机器**，不是期望值。

消费者：`crates/core/tests/capture_globals_configs.rs`（票据 #17 的独立验证）。
那个文件用 `toml::from_str::<MachineFixture>(&std::fs::read_to_string(...)?)` 读它们，
再交给 `tuoen_core::capture::test_support::CaptureFixture::build` 造一台**假机器**。

## 一条规矩：期望值是**数出来的**

固定装置里**不写期望值**，也不写"应该有几条"。测试自己从这些文件里数：

- 包数 → 从 `npm ls -g --json` / `pip list --format=json` 的 **JSON 文本**里数；
- 跳过项 → 从声明的路径按决策 179/180/181/182 的**判据表**推出来；
- 哈希 → 测试自己对该文件的 `content` 算 sha256；
- `prefix_inside_version_dir` → 测试自己按决策 173 的四支判据判。

**绝不从产品输出反推期望** —— 那等于拿产品自己的输出当答案，两边一起错的时候它永远是绿的。

## 目录约定

一个场景一个目录；目录里**每个 `*.toml` 是一台独立的机器**（文件名说清它是什么形态）。
只有一个形态的场景用 `machine.toml`；同一场景要多种形态时用多个文件
（例：`enumeration-fails/` 有三种失败形态，`git-identity-layers/` 有五种层级形态）。
测试按**文件名字典序**枚举目录里的 `*.toml`，所以文件名也是失败信息的顺序。

| 目录 | 测什么（决策） |
|---|---|
| `npm-inside-version-dir/` | 173：`prefix_inside_version_dir` 的四支判据**各自单独**验一遍 —— `machine.toml` ①+④ 为真（npm 的 prefix 是联接、目标是版本目录）而同机的 pip 四支全假；`version-segment-prefix.toml` **只有 ③** 为真（普通目录，路径里有 `v24.19.0`）；`ancestor-junction.toml` **只有 ②** 为真（prefix 自己是普通目录，祖先才是联接）。后两份里除那一条判据之外没有任何 reparse point —— 只判 reparse 的实现会在它们上面红 |
| `two-node-versions/` | 172/174：同一个 prefix 在两个 Node 版本下 → **两份独立清单**（`tool_version` 与包集合都不同） |
| `enumeration-fails/` | 175：`command-failed` / `timed-out` / `bad-json` 三种 → **行仍然写出去**（带 prefix 与 tool_version），且 `capture` 本身不失败 |
| `git-identity-layers/` | 183：系统级 / 全局级 / 两层都有（全局赢）/ 两层都没有（`missing`）/ git 不可用（`unknown`） |
| `credential-shape/` | 178：`.m2/settings.xml` 里的假 GitLab PAT（前缀 + 16 位）→ 跳过，且**材料一个字节都不进快照**（本票安全红线） |
| `unreadable-too-large-binary/` | 179：`unreadable` / `too-large` / `binary` 三种 `skip_reason` 各一条，外加一条**必须被捕获**的对照 |
| `caches-and-keys/` | 179/180/181/182：六个缓存目录（其中一个声明为不存在）+ `.ssh` 私钥形状 + JetBrains 凭据库/私钥 + 产品目录标记行 |
| `idempotent/` | 幂等：同一份装置、同一个 `captured_at` 跑两次 → 逐字节相同 |

## 假 token 的写法（本票安全红线）

`credential-shape/machine.toml` 里那个 PAT 是**假**的，但它必须**形状正确**（否则测不到
决策 178 的内容级判据）。写法：

```toml
value = "glpat\u002D0000000000000000"
```

`\u002D` 是 `-`。于是**文件文本里没有那个前缀的字面量**（GitHub push protection
GH013 在本仓库拦过一次这种形状），而**解析出来的值有** —— 判据看到的是真形状。
测试里比对时同样用 `concat!("glpat", "-")` 拼，不写字面量。

## 三条硬规矩（都是踩过的）

1. **字符串一律写成单行基本字符串 + 转义**：`content = "a\nb\tc"`，**不要**用 `"""…"""`。
   理由：`.gitattributes` 是 `*.toml text` 而本机 `core.autocrlf=true` ⇒ 工作树是 **CRLF**，
   而 `toml` crate **不**把多行字符串里的 `\r\n` 归一化 —— 于是解析出来的长度会比仓库里的
   LF 版多"换行数"个字节（实测：`.npmrc` 声明 54 字节，解析出来 56），直接撞上决策 186 的
   `content.is_some() ⇒ size == content.len()`。单行 + `\n` 转义与工作树行尾**无关**。
   `stdout` 同理：git 的输出是按行解析的，行尾的 `\r` 会进到值里。
2. **`size` 必须等于 `content` 的字节数**（决策 186 的构造期校验会 panic 并印出路径与两个数）。
3. **假 token 的前缀用 `\u002D` 写那个连字符**（GH013 拦过一次），见上一节。

## 采集器与固定装置之间的接口（已与 `restore-core`（task-7）逐字对齐）

- **`[[processes]]` 的 `program` 一律是裸名字**（`node.exe` / `cmd.exe` / `pip.exe` /
  `git.exe`），`FixtureProcess.program` 走 `normalize_path`（大小写、分隔符不敏感）。
  逐字表（调用顺序也是这个）：

  | 用途 | program | args |
  |---|---|---|
  | npm 的 `tool_version` | `node.exe` | `-v` |
  | npm 的 prefix | `cmd.exe` | `/C` `npm.cmd` `config` `get` `prefix` |
  | npm 包清单 | `cmd.exe` | `/C` `npm.cmd` `ls` `-g` `--json` `--depth=0` `--offline` |
  | pip 的 `tool_version` + prefix | `pip.exe` | `--version` |
  | pip 包清单 | `pip.exe` | `list` `--format=json` `--disable-pip-version-check` |
  | git 系统级 | `git.exe` | `config` `--system` `--list` `--show-origin` |
  | git 全局级 | `git.exe` | `config` `--global` `--list` `--show-origin` |

- **`args` 的匹配语义（决策 185，`pathrewrite-platform` 已落地）**：
  ① `program` 归一化后相等；② 且声明的 `args` 为空（**通配**）**或** 调用 args 以它开头
  （**逐元素、顺序敏感**，允许调用更长 —— 所以 `npm.cmd-extra` 不会命中 `npm.cmd`）；
  ③ 多个命中时 **`args` 最长的赢**；④ 同长按**声明顺序**取第一个。
  调试提示：没命中时 `spawn_error` 分两句 —— "固定装置里没有这个程序" 与
  "有这个程序，但没有一条的 `args` 是这次调用的前缀"。看到**后者**就去查 args 的
  顺序/拼写，别去查 program。
  **已知代价（写在这里，因为它最容易咬人）**：给同一个 `program` **新增一条更长的
  `args` 条目**，会让原来靠"空 args 兜底"命中的短条目**静默不再命中** ——
  最长的那条赢了，而原来那条还在文件里、看起来仍然有效。所以在同一份装置里加条目时，
  要么给旧条目也写上 `args`，要么确认再没有人需要它兜底。本目录里 `cmd.exe` 那两条
  就是"两条都写精确 args、没有兜底"的形态（`npm.cmd config get prefix` 与
  `npm.cmd ls …` 只有 args 不同）。
- **"工具存在"的判据**：在**进程** `Path` 上逐目录 `list_dir` 找 `npm.cmd` / `pip.exe`
  （大小写不敏感）—— 所以这两个文件必须**真的声明在 PATH 上的某个目录里**，
  否则**整行不会出现**（那不是"枚举失败"，是"没这个工具"）。`cmd.exe` 不查存在；
  `node.exe` 只影响 npm 行的 `tool_version`（找不到 → `"unknown"`，行照出）。
- **读文件内容走 `ctx.fs.read(path, MAX_CONFIG_BYTES)`**（`MAX_CONFIG_BYTES = 256 * 1024`），
  所以内容写在 `FixturePath.content` 上；四态映射：读到 → 算 `sha256:<hex 小写>` + `bytes`；
  太大 → `too-large`；读不出来 → `unreadable`；**不存在 → 什么都不出**（不是跳过项）。
- **采集器只读这四个 env 变量**：`USERPROFILE`、`APPDATA`、`LOCALAPPDATA`、`Path`。
  **注册表一个键都不读**（`env` 那一节才读）。

  候选表逐字（**以产品的候选表为准**：`crates/core/src/capture/collect/configs.rs`
  的 `FILE_CANDIDATES`，7 条）：

  | 根 | 后缀 | kind | layer |
  |---|---|---|---|
  | `USERPROFILE` | `\.gitconfig` | `git` | `global` |
  | `USERPROFILE` | `\.npmrc` | `npm` | — |
  | `USERPROFILE` | `\.m2\settings.xml` | `maven` | — |
  | `USERPROFILE` | `\.docker\config.json` | `docker` | — |
  | `USERPROFILE` | `\.ssh\config` | `ssh` | — |
  | `USERPROFILE` | `\.wslconfig` | `wsl` | — |
  | `APPDATA` | `\Code\User\settings.json` | `vscode` | — |

  最后两条（`wsl` / `vscode`）**真机上都不存在**（没装 WSL 发行版配置、没装 vscode）⇒
  产品走 `NotFound` 分支、什么都不出 —— 所以它们**目前没有任何端到端证据**，
  固定装置里也**没有声明**它们（声明一份不存在的机器只会测出一个恒真的空结论）。
  另：`layer` 只在**有层级概念**的文件上出键（只有 git 的两层）。
- **git 的系统级那一份不在上面这张表里**：它的路径只能从
  `git config --system --list --show-origin` 的第一行 `file:` 里拿（kind `git`、
  `layer = "system"`），所以 git 不可用时**连这条行都没有** ——
  而 `~/.gitconfig` 那条**照旧在**（它是 env 候选，与 git 可不可用无关）。
- **`--show-origin` 的路径原样存**（只去掉 `file:` 前缀，不做 `/` → `\` 归一化）——
  存"工具说了什么"。所以 `file:C:/Program Files/Git/etc/gitconfig\tcore.autocrlf=true`
  的期望值就是 `C:/Program Files/Git/etc/gitconfig`。
- **JetBrains 的 kind 映射**：`c.kdbx` / `c.pwd` → `credential-database`；
  `idea.key` → `private-key-file`；产品目录本身 → 标记行（`kind = "jetbrains"`、
  `captured = true`、**没有 `bytes` / `content_hash`**）。

## 仍然是我的假设（对齐后要复核，写进交付报告）

- **pip 的 prefix** 来自 `pip --version` 里 `from <路径>` 那条路径的安装根
  （`C:\Python312\Lib\site-packages\pip` → `C:\Python312`）。这是"一真一假同机"里
  第二个 prefix 的唯一来源；若产品不产出它，那是**差异**，会被用例抓住。
- **`binary` 的判据是内容里有 NUL**（`content` 是 `String`，非法 UTF-8 表达不出来），
  不是按扩展名。
- **`unreadable` 的表达方式是"声明存在与大小、但没有 `content`"** —— 假文件系统不肯替
  我们编内容（`content.is_none()` 就是 `ReadOutcome::Unreadable`）。
- **`too-large` 只能由"内容长度超过 limit"表达**（`read(path, 262144)` 的判据是
  `content.len() > limit`，**声明的 `size` 不参与**）—— 所以固定装置里那条只写
  `size = 262145`，**内容由测试在 Rust 里补成等长字符串**（`with_synthetic_large_content()`）：
  256 KiB 的填充不是信息，不该进仓库、也不该让 `git diff` 不可看。
  反过来说：只写 `size` 而不写 `content` 会落到 `unreadable`，**不会**落到 `too-large`。
- `size` 与 `content` 的字节数**逐条相等**（脚本核对过）；产品的 `bytes` 应当来自读到的内容。
